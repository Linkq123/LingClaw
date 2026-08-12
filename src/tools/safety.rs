//! Safety helpers for the CLI/Tools layer: dangerous-command detection and
//! workspace-scoped path resolution. These are consumed by the tool
//! implementations in `src/tools/` (and exercised by tests); the HTTP/WS
//! server core in `main.rs` does not call them directly.

use std::{
    ffi::OsStr,
    fs::File,
    path::{Component, Path, PathBuf},
};

const DANGEROUS_PATTERNS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "rm -rf ~",
    "mkfs.",
    "dd if=/dev",
    ":(){ :|:&",
    "> /dev/sda",
    "chmod -r 777 /",
    "chown -r root",
    "format c:",
    "del /f /s /q c:\\",
    "rd /s /q c:\\",
    "reg delete hk",
];

/// Collapse repeated whitespace to a single space for robust pattern matching.
fn normalize_command_whitespace(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn check_dangerous_command(cmd: &str) -> Option<&'static str> {
    let lower = normalize_command_whitespace(cmd).to_lowercase();
    DANGEROUS_PATTERNS
        .iter()
        .find(|&&pattern| lower.contains(pattern))
        .copied()
}

#[cfg(windows)]
fn windows_user_path(path: &Path) -> PathBuf {
    let value = path.to_string_lossy();
    if let Some(path) = value.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{path}"));
    }
    if let Some(path) = value.strip_prefix(r"\\?\") {
        return PathBuf::from(path);
    }
    path.to_path_buf()
}

#[cfg(windows)]
fn windows_component_eq(left: Component<'_>, right: Component<'_>) -> bool {
    match (left, right) {
        (Component::Prefix(left), Component::Prefix(right)) => {
            left.as_os_str().to_string_lossy().to_lowercase()
                == right.as_os_str().to_string_lossy().to_lowercase()
        }
        (Component::RootDir, Component::RootDir)
        | (Component::CurDir, Component::CurDir)
        | (Component::ParentDir, Component::ParentDir) => true,
        (Component::Normal(left), Component::Normal(right)) => {
            left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
        }
        _ => false,
    }
}

fn strip_workspace_prefix(path: &Path, workspace: &Path) -> Option<PathBuf> {
    if let Ok(relative) = path.strip_prefix(workspace) {
        return Some(relative.to_path_buf());
    }

    #[cfg(windows)]
    {
        let path = windows_user_path(path);
        let workspace = windows_user_path(workspace);
        let mut path_components = path.components();
        for workspace_component in workspace.components() {
            let path_component = path_components.next()?;
            if !windows_component_eq(path_component, workspace_component) {
                return None;
            }
        }
        Some(path_components.as_path().to_path_buf())
    }

    #[cfg(not(windows))]
    None
}

fn is_bootstrap_component(part: &OsStr) -> bool {
    #[cfg(windows)]
    {
        part.to_string_lossy()
            .eq_ignore_ascii_case(".lingclaw-bootstrap")
    }
    #[cfg(not(windows))]
    {
        part == ".lingclaw-bootstrap"
    }
}

pub(crate) fn resolve_path_checked(path_str: &str, workspace: &Path) -> Result<PathBuf, String> {
    let workspace_root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let raw = Path::new(path_str);
    let relative = if raw.is_absolute() {
        if let Some(relative) = strip_workspace_prefix(raw, workspace) {
            relative
        } else if let Some(relative) = strip_workspace_prefix(raw, &workspace_root) {
            relative
        } else if let Ok(canonical_raw) = raw.canonicalize() {
            strip_workspace_prefix(&canonical_raw, &workspace_root).ok_or_else(|| {
                format!(
                    "path '{}' is outside the session workspace '{}'",
                    path_str,
                    workspace_root.display()
                )
            })?
        } else {
            return Err(format!(
                "path '{}' is outside the session workspace '{}'",
                path_str,
                workspace_root.display()
            ));
        }
    } else {
        raw.to_path_buf()
    };

    if relative.components().any(
        |component| matches!(component, Component::Normal(part) if is_bootstrap_component(part)),
    ) {
        return Err(format!(
            "path '{}' targets protected internal workspace data",
            path_str
        ));
    }

    let mut resolved = workspace_root.clone();
    for component in relative.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if resolved == workspace_root {
                    return Err(format!(
                        "path '{}' is outside the session workspace '{}'",
                        path_str,
                        workspace_root.display()
                    ));
                }
                resolved.pop();
            }
            std::path::Component::Normal(part) => {
                let candidate = resolved.join(part);
                if let Ok(meta) = std::fs::symlink_metadata(&candidate)
                    && meta.file_type().is_symlink()
                {
                    let escaped_workspace = candidate
                        .canonicalize()
                        .ok()
                        .is_some_and(|target| !path_is_within_workspace(&target, &workspace_root));
                    return Err(format!(
                        "path '{}' traverses symlink '{}'{}outside the session workspace '{}'",
                        path_str,
                        candidate.display(),
                        if escaped_workspace {
                            " that resolves "
                        } else {
                            " "
                        },
                        workspace_root.display()
                    ));
                }
                if let Ok(canon) = candidate.canonicalize()
                    && !path_is_within_workspace(&canon, &workspace_root)
                {
                    return Err(format!(
                        "path '{}' resolves outside the session workspace '{}' via '{}'",
                        path_str,
                        workspace_root.display(),
                        candidate.display()
                    ));
                }
                resolved = candidate;
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(format!(
                    "path '{}' is outside the session workspace '{}'",
                    path_str,
                    workspace_root.display()
                ));
            }
        }
    }

    Ok(resolved)
}

fn open_file_no_follow(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn opened_file_path(file: &File, _: &Path) -> std::io::Result<PathBuf> {
    use std::os::fd::AsRawFd as _;

    std::fs::canonicalize(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn opened_file_path(file: &File, _: &Path) -> std::io::Result<PathBuf> {
    use std::{
        ffi::CStr,
        os::{fd::AsRawFd as _, unix::ffi::OsStrExt as _},
    };

    let mut buffer = vec![0i8; libc::PATH_MAX as usize];
    // SAFETY: `buffer` is writable for PATH_MAX bytes and `file` owns a valid descriptor.
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: F_GETPATH writes a NUL-terminated path into the supplied PATH_MAX buffer.
    let path = unsafe { CStr::from_ptr(buffer.as_ptr()) };
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios"
    ))
))]
fn opened_file_path(file: &File, _: &Path) -> std::io::Result<PathBuf> {
    use std::os::fd::AsRawFd as _;

    std::fs::canonicalize(format!("/dev/fd/{}", file.as_raw_fd()))
}

#[cfg(windows)]
fn opened_file_path(file: &File, _: &Path) -> std::io::Result<PathBuf> {
    use std::os::{windows::ffi::OsStringExt as _, windows::io::AsRawHandle as _};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FileAttributeTagInfo,
        GetFileInformationByHandleEx, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    let handle = file.as_raw_handle();
    let mut tag_info = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: `handle` remains valid for this call and `tag_info` is a correctly sized output.
    let info_ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            (&mut tag_info as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if info_ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if tag_info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "path resolves through a reparse point",
        ));
    }

    let mut buffer = vec![0u16; 32_768];
    // SAFETY: `handle` is valid and `buffer` is writable for the supplied length.
    let mut length = unsafe {
        GetFinalPathNameByHandleW(
            handle,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            VOLUME_NAME_DOS,
        )
    };
    if length == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if length as usize >= buffer.len() {
        buffer.resize(length as usize + 1, 0);
        // SAFETY: same valid handle with the resized writable buffer.
        length = unsafe {
            GetFinalPathNameByHandleW(
                handle,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                VOLUME_NAME_DOS,
            )
        };
        if length == 0 || length as usize >= buffer.len() {
            return Err(std::io::Error::last_os_error());
        }
    }
    let raw = std::ffi::OsString::from_wide(&buffer[..length as usize]);
    let raw = raw.to_string_lossy();
    let normalized = raw
        .strip_prefix(r"\\?\UNC\")
        .map(|path| format!(r"\\{path}"))
        .or_else(|| raw.strip_prefix(r"\\?\").map(str::to_string))
        .unwrap_or_else(|| raw.into_owned());
    Ok(PathBuf::from(normalized))
}

#[cfg(not(any(unix, windows)))]
fn opened_file_path(_: &File, requested: &Path) -> std::io::Result<PathBuf> {
    requested.canonicalize()
}

#[cfg(windows)]
fn path_is_within_workspace(path: &Path, workspace: &Path) -> bool {
    fn normalize(path: &Path) -> String {
        let raw = path.to_string_lossy();
        let raw = raw
            .strip_prefix(r"\\?\UNC\")
            .map(|path| format!(r"\\{path}"))
            .or_else(|| raw.strip_prefix(r"\\?\").map(str::to_string))
            .unwrap_or_else(|| raw.into_owned());
        raw.replace('/', "\\").trim_end_matches('\\').to_lowercase()
    }

    let path = normalize(path);
    let workspace = normalize(workspace);
    path == workspace
        || path
            .strip_prefix(&workspace)
            .is_some_and(|suffix| suffix.starts_with('\\'))
}

#[cfg(not(windows))]
fn path_is_within_workspace(path: &Path, workspace: &Path) -> bool {
    path.starts_with(workspace)
}

/// Open an already-resolved workspace file without following the final link,
/// then verify the opened handle still points beneath the bound workspace.
/// Keeping the handle open closes the metadata/read TOCTOU window for callers.
pub(crate) fn open_checked_workspace_file(
    resolved: &Path,
    workspace_root: &Path,
) -> Result<(File, u64), String> {
    let file = open_file_no_follow(resolved)
        .map_err(|error| format!("cannot read '{}': {error}", resolved.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect '{}': {error}", resolved.display()))?;
    if !metadata.is_file() {
        return Err(format!("'{}' is not a file", resolved.display()));
    }
    let actual_path = opened_file_path(&file, resolved)
        .map_err(|error| format!("cannot verify '{}': {error}", resolved.display()))?;
    if !path_is_within_workspace(&actual_path, workspace_root) {
        return Err(format!(
            "path '{}' resolves outside the session workspace '{}'",
            resolved.display(),
            workspace_root.display()
        ));
    }
    Ok((file, metadata.len()))
}
