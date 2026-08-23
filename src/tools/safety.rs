//! Safety helpers for the CLI/Tools layer: dangerous-command detection and
//! workspace-scoped path resolution. These are consumed by the tool
//! implementations in `src/tools/` (and exercised by tests); the HTTP/WS
//! server core in `main.rs` does not call them directly.

use std::{
    ffi::OsStr,
    fs::{File, Metadata},
    path::{Component, Path, PathBuf},
    sync::Arc,
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

pub(crate) fn resolve_path_checked(
    path_str: &str,
    workspace: &Path,
) -> Result<CheckedWorkspacePath, String> {
    resolve_path_checked_with_probe_hook(path_str, workspace, &mut || {})
}

fn resolve_path_checked_with_probe_hook(
    path_str: &str,
    workspace: &Path,
    probe_hook: &mut dyn FnMut(),
) -> Result<CheckedWorkspacePath, String> {
    let workspace_anchor = Arc::new(open_checked_workspace_root(workspace)?);
    let workspace_root = workspace_anchor.requested.clone();
    let raw = Path::new(path_str);
    let relative = if raw.is_absolute() {
        if let Some(relative) = strip_workspace_prefix(raw, workspace) {
            relative
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
                resolved.push(part);
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

    let checked_relative = strip_workspace_prefix(&resolved, &workspace_root).ok_or_else(|| {
        format!(
            "path '{}' is outside the session workspace '{}'",
            path_str,
            workspace_root.display()
        )
    })?;
    let binding = bind_workspace_namespace_with_probe_hook(
        &workspace_anchor,
        &checked_relative,
        &resolved,
        probe_hook,
    )?;
    let checked = CheckedWorkspacePath {
        root: workspace_anchor,
        relative: checked_relative,
        requested: resolved,
        namespace: binding.namespace,
        suffix: binding.suffix,
        probe: binding.probe,
    };
    checked.validate()?;
    Ok(checked)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_file_no_follow(path: &Path) -> std::io::Result<File> {
    use std::{
        ffi::CString,
        os::{fd::FromRawFd as _, unix::ffi::OsStrExt as _},
    };

    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace root contains a NUL byte",
        )
    })?;
    // O_PATH establishes a namespace capability without requiring data-read
    // permission. O_DIRECTORY and O_NOFOLLOW reject a replaced root link.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful open returns one newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn open_windows_absolute_no_follow(
    path: &Path,
    desired_access: u32,
    share_delete: bool,
) -> std::io::Result<File> {
    use std::os::{windows::ffi::OsStrExt as _, windows::io::FromRawHandle as _};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
    };

    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide.push(0);
    let share =
        FILE_SHARE_READ | FILE_SHARE_WRITE | if share_delete { FILE_SHARE_DELETE } else { 0 };
    // SAFETY: the UTF-16 path is NUL-terminated and remains live for the call.
    // FILE_TRAVERSE retains directory namespace identity without imposing
    // FILE_READ_DATA/LIST_DIRECTORY on later write/delete/cwd operations.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            desired_access,
            share,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned one newly owned handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(windows)]
fn open_file_no_follow(path: &Path) -> std::io::Result<File> {
    // This is the long-lived root trust anchor, not a generic target probe.
    // Denying delete sharing keeps the persisted root pathname stable for APIs
    // such as CreateProcess that can accept only a cwd string, while child
    // probes and data handles continue to share delete access.
    open_windows_absolute_no_follow(
        path,
        windows_sys::Win32::Storage::FileSystem::FILE_TRAVERSE,
        false,
    )
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn open_file_no_follow(_: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure mount-bound workspace capabilities are unavailable on this Unix platform",
    ))
}

#[cfg(not(any(unix, windows)))]
fn open_file_no_follow(_: &Path) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure workspace capabilities are unavailable on this platform",
    ))
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
    Ok(PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..length as usize],
    )))
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

fn metadata_is_link_or_reparse(metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        metadata.file_type().is_symlink()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

fn paths_are_same(left: &Path, right: &Path) -> bool {
    path_is_within_workspace(left, right) && path_is_within_workspace(right, left)
}

#[derive(Debug)]
struct CheckedWorkspaceRoot {
    file: File,
    requested: PathBuf,
}

#[derive(Debug)]
struct CheckedWorkspaceNamespace {
    file: File,
    relative: PathBuf,
}

struct WorkspaceNamespaceBinding {
    namespace: Arc<CheckedWorkspaceNamespace>,
    suffix: PathBuf,
    probe: Option<Arc<File>>,
}

/// A lexical workspace-relative path bound to one unfollowed persisted-root
/// handle. Callers must perform filesystem work through this capability; the
/// display path is never an authorization token.
#[derive(Clone, Debug)]
pub(crate) struct CheckedWorkspacePath {
    root: Arc<CheckedWorkspaceRoot>,
    relative: PathBuf,
    requested: PathBuf,
    namespace: Arc<CheckedWorkspaceNamespace>,
    suffix: PathBuf,
    probe: Option<Arc<File>>,
}

impl CheckedWorkspacePath {
    pub(crate) fn display_path(&self) -> &Path {
        &self.requested
    }

    pub(crate) fn relative_path(&self) -> &Path {
        &self.relative
    }

    pub(crate) fn file_name(&self) -> Option<&OsStr> {
        self.relative.file_name()
    }

    /// Revalidate the persisted workspace root, retained namespace, and any
    /// existing target identity without reopening the display path as an
    /// authorization token.
    pub(crate) fn validate(&self) -> Result<(), String> {
        verify_checked_workspace_capability(self)
    }

    pub(crate) fn parent(&self) -> Result<Self, String> {
        verify_checked_workspace_capability(self)?;
        let relative = self
            .relative
            .parent()
            .ok_or_else(|| "the workspace root has no workspace parent".to_string())?
            .to_path_buf();
        let suffix = self.suffix.parent().ok_or_else(|| {
            "the retained workspace namespace does not expose its parent".to_string()
        })?;
        let probe = if suffix.as_os_str().is_empty() {
            Some(Arc::new(self.namespace.file.try_clone().map_err(
                |error| checked_relative_open_error(&self.requested, error),
            )?))
        } else {
            None
        };
        Ok(Self {
            root: Arc::clone(&self.root),
            requested: self.root.requested.join(&relative),
            relative,
            namespace: Arc::clone(&self.namespace),
            suffix: suffix.to_path_buf(),
            probe,
        })
    }

    pub(crate) fn child(&self, name: &OsStr) -> Result<Self, String> {
        verify_checked_workspace_capability(self)?;
        if Path::new(name).components().count() != 1
            || !matches!(
                Path::new(name).components().next(),
                Some(Component::Normal(_))
            )
            || is_bootstrap_component(name)
        {
            return Err("workspace child name is not one safe path component".to_string());
        }
        let mut relative = self.relative.clone();
        relative.push(name);
        let parent_entry = self
            .open_entry()?
            .ok_or_else(|| "workspace child parent no longer exists".to_string())?;
        if !parent_entry.metadata.is_dir() {
            return Err(format!("'{}' is not a directory", self.requested.display()));
        }
        let directory = reopen_workspace_directory_namespace(&parent_entry.file)
            .map_err(|error| checked_relative_open_error(&self.requested, error))?;
        let target = self.root.requested.join(&relative);
        let probe = probe_workspace_target_component(&directory, name, &target)
            .map_err(|error| {
                checked_relative_open_error(&self.root.requested.join(&relative), error)
            })?
            .map(Arc::new);
        Ok(Self {
            root: Arc::clone(&self.root),
            requested: self.root.requested.join(&relative),
            relative,
            namespace: Arc::new(CheckedWorkspaceNamespace {
                file: directory,
                relative: self.relative.clone(),
            }),
            suffix: PathBuf::from(name),
            probe,
        })
    }

    pub(crate) fn open_entry(&self) -> Result<Option<CheckedWorkspaceEntry>, String> {
        verify_checked_workspace_capability(self)?;
        open_checked_workspace_entry_with_hook(self, &mut || {})
    }

    pub(crate) fn open_file_for_read(&self) -> Result<(CheckedWorkspaceFile, u64), String> {
        verify_checked_workspace_capability(self)?;
        let file = open_workspace_file_for_read(self)?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err(format!("'{}' is not a file", self.requested.display()));
        }
        Ok((file, metadata.len()))
    }

    pub(crate) fn open_file_entry_for_read(&self) -> Result<CheckedWorkspaceEntry, String> {
        verify_checked_workspace_capability(self)?;
        let file = open_workspace_file_for_read(self)?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err(format!("'{}' is not a file", self.requested.display()));
        }
        Ok(CheckedWorkspaceEntry {
            file: file.file,
            metadata,
            _operation_anchors: file.anchors,
            root: Arc::clone(&self.root),
            relative: self.relative.clone(),
            requested: self.requested.clone(),
        })
    }

    pub(crate) fn open_file_for_write(&self) -> Result<CheckedWorkspaceFile, String> {
        verify_checked_workspace_capability(self)?;
        open_workspace_file_for_write(self, true, &mut || {})
    }

    pub(crate) fn open_file_for_patch(&self) -> Result<CheckedWorkspaceFile, String> {
        verify_checked_workspace_capability(self)?;
        open_workspace_file_for_patch(self)
    }

    pub(crate) fn remove_file(&self) -> Result<(), String> {
        verify_checked_workspace_capability(self)?;
        remove_workspace_file(self, &mut || {})
    }

    pub(crate) fn read_directory(&self) -> Result<Vec<CheckedWorkspaceDirEntry>, String> {
        verify_checked_workspace_capability(self)?;
        let entry = self
            .open_entry()?
            .filter(|entry| entry.metadata.is_dir())
            .ok_or_else(|| format!("'{}' is not a directory", self.requested.display()))?;
        read_workspace_directory_from_entry(self, &entry)
    }

    pub(crate) fn read_directory_from_entry(
        &self,
        entry: &CheckedWorkspaceEntry,
    ) -> Result<Vec<CheckedWorkspaceDirEntry>, String> {
        verify_checked_workspace_capability(self)?;
        reverify_checked_workspace_entry(entry, self)?;
        read_workspace_directory_from_entry(self, entry)
    }

    pub(crate) fn process_cwd(&self) -> Result<CheckedWorkspaceCwd, String> {
        verify_checked_workspace_capability(self)?;
        checked_workspace_process_cwd(self)
    }

    /// Export this checked directory to a stdio child without converting the
    /// capability back into a mutable host pathname. Linux/Android children
    /// inherit a dedicated directory fd and receive `/proc/self/fd/<n>`;
    /// Windows keeps a no-delete root lock alive while the child uses the
    /// ordinary file URI path.
    pub(crate) fn export_to_child(&self) -> Result<CheckedWorkspaceChildRoot, String> {
        let cwd = self.process_cwd()?;
        checked_workspace_child_root(cwd)
    }

    #[cfg(test)]
    fn open_file_for_write_after_lock_hook(
        &self,
        hook: &mut dyn FnMut(),
    ) -> Result<CheckedWorkspaceFile, String> {
        verify_checked_workspace_capability(self)?;
        open_workspace_file_for_write(self, true, hook)
    }

    #[cfg(test)]
    fn remove_file_after_lock_hook(&self, hook: &mut dyn FnMut()) -> Result<(), String> {
        verify_checked_workspace_capability(self)?;
        remove_workspace_file(self, hook)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckedWorkspaceDirEntryKind {
    File,
    Directory,
    LinkOrReparse,
    Other,
    Missing,
}

pub(crate) struct CheckedWorkspaceDirEntry {
    pub(crate) name: std::ffi::OsString,
    pub(crate) path: CheckedWorkspacePath,
    pub(crate) kind: CheckedWorkspaceDirEntryKind,
    pub(crate) metadata: Option<Metadata>,
}

#[derive(Debug)]
pub(crate) struct CheckedWorkspaceCwd {
    path: PathBuf,
    source: CheckedWorkspacePath,
    anchors: Vec<WorkspaceIdentityAnchor>,
}

#[derive(Debug)]
pub(crate) struct CheckedWorkspaceChildRoot {
    path: PathBuf,
    cwd: CheckedWorkspaceCwd,
    #[cfg(any(target_os = "linux", target_os = "android"))]
    _inherited_file: File,
}

impl CheckedWorkspaceChildRoot {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub(crate) fn inherited_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd as _;

        self._inherited_file.as_raw_fd()
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        self.cwd.validate()
    }
}

#[derive(Debug)]
struct WorkspaceIdentityAnchor {
    file: File,
    expected: PathBuf,
}

/// An operation handle that keeps its namespace anchors alive and revalidates
/// them before every read, write, seek, or truncation. Windows anchors deny
/// delete sharing for the operation lifetime; Linux/Android anchors provide a
/// fresh openat2-bound namespace snapshot and fail closed when it moves.
#[derive(Debug)]
pub(crate) struct CheckedWorkspaceFile {
    file: File,
    path: CheckedWorkspacePath,
    anchors: Vec<WorkspaceIdentityAnchor>,
}

impl CheckedWorkspaceFile {
    fn new(
        path: &CheckedWorkspacePath,
        file: File,
        anchors: Vec<WorkspaceIdentityAnchor>,
    ) -> Result<Self, String> {
        let checked = Self {
            file,
            path: path.clone(),
            anchors,
        };
        checked.validate().map_err(|error| error.to_string())?;
        Ok(checked)
    }

    fn validate(&self) -> std::io::Result<()> {
        self.path.validate().map_err(std::io::Error::other)?;
        for anchor in &self.anchors {
            let actual = opened_file_path(&anchor.file, &anchor.expected)?;
            if !paths_are_same(&actual, &anchor.expected) {
                return Err(std::io::Error::other(format!(
                    "workspace operation anchor '{}' moved after it was opened",
                    anchor.expected.display()
                )));
            }
        }
        #[cfg(not(windows))]
        {
            let actual = opened_file_path(&self.file, self.path.display_path())?;
            if !paths_are_same(&actual, self.path.display_path()) {
                return Err(std::io::Error::other(format!(
                    "workspace operation target '{}' moved after it was opened",
                    self.path.display_path().display()
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn metadata(&self) -> std::io::Result<Metadata> {
        self.validate()?;
        self.file.metadata()
    }

    pub(crate) fn set_len(&self, size: u64) -> std::io::Result<()> {
        self.validate()?;
        self.file.set_len(size)
    }
}

impl std::io::Read for CheckedWorkspaceFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.validate()?;
        std::io::Read::read(&mut self.file, buffer)
    }
}

impl std::io::Write for CheckedWorkspaceFile {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.validate()?;
        std::io::Write::write(&mut self.file, buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.validate()?;
        std::io::Write::flush(&mut self.file)
    }
}

impl std::io::Seek for CheckedWorkspaceFile {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.validate()?;
        std::io::Seek::seek(&mut self.file, position)
    }
}

impl CheckedWorkspaceCwd {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        self.source.validate()?;
        validate_operation_anchors(&self.anchors)
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn checked_workspace_child_root(
    cwd: CheckedWorkspaceCwd,
) -> Result<CheckedWorkspaceChildRoot, String> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    cwd.validate()?;
    let directory = cwd
        .anchors
        .last()
        .ok_or_else(|| "workspace child-root directory capability was lost".to_string())?;
    // Keep the parent copy close-on-exec so unrelated children cannot inherit
    // this workspace authority. The MCP spawn path clears FD_CLOEXEC only in
    // its own post-fork child. Start above ordinary stdio fds to avoid a
    // collision with Command's pipe setup.
    // SAFETY: fcntl duplicates one live directory descriptor on success.
    let fd = unsafe { libc::fcntl(directory.file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
    if fd == -1 {
        return Err(format!(
            "cannot export checked workspace root to child: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: successful F_DUPFD_CLOEXEC returned one newly owned descriptor.
    let inherited_file = unsafe { File::from_raw_fd(fd) };
    cwd.validate()?;
    Ok(CheckedWorkspaceChildRoot {
        path: PathBuf::from(format!("/proc/self/fd/{fd}")),
        cwd,
        _inherited_file: inherited_file,
    })
}

#[cfg(windows)]
fn checked_workspace_child_root(
    cwd: CheckedWorkspaceCwd,
) -> Result<CheckedWorkspaceChildRoot, String> {
    cwd.validate()?;
    Ok(CheckedWorkspaceChildRoot {
        path: cwd.path.clone(),
        cwd,
    })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn checked_workspace_child_root(
    _: CheckedWorkspaceCwd,
) -> Result<CheckedWorkspaceChildRoot, String> {
    unsupported_unix_operation()
}

#[cfg(not(any(unix, windows)))]
fn checked_workspace_child_root(
    _: CheckedWorkspaceCwd,
) -> Result<CheckedWorkspaceChildRoot, String> {
    Err("secure workspace export to child is unavailable on this platform".into())
}

pub(crate) struct CheckedWorkspaceEntry {
    pub(crate) file: File,
    pub(crate) metadata: Metadata,
    _operation_anchors: Vec<WorkspaceIdentityAnchor>,
    root: Arc<CheckedWorkspaceRoot>,
    relative: PathBuf,
    requested: PathBuf,
}

fn open_checked_workspace_root(workspace_root: &Path) -> Result<CheckedWorkspaceRoot, String> {
    if !workspace_root.is_absolute() {
        return Err(format!(
            "session workspace '{}' is not an absolute persisted root",
            workspace_root.display()
        ));
    }
    let file = open_file_no_follow(workspace_root).map_err(|error| {
        format!(
            "cannot open session workspace root '{}' without following links: {error}",
            workspace_root.display()
        )
    })?;
    let metadata = file.metadata().map_err(|error| {
        format!(
            "cannot inspect session workspace root '{}': {error}",
            workspace_root.display()
        )
    })?;
    if metadata_is_link_or_reparse(&metadata) {
        return Err(format!(
            "session workspace root '{}' is a link or reparse point",
            workspace_root.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "session workspace root '{}' is not a directory",
            workspace_root.display()
        ));
    }
    let actual = opened_file_path(&file, workspace_root).map_err(|error| {
        format!(
            "cannot verify session workspace root '{}': {error}",
            workspace_root.display()
        )
    })?;
    if !paths_are_same(&actual, workspace_root) {
        return Err(format!(
            "session workspace root '{}' changed while its trust anchor was opened",
            workspace_root.display()
        ));
    }
    Ok(CheckedWorkspaceRoot {
        file,
        requested: workspace_root.to_path_buf(),
    })
}

fn verify_checked_workspace_root(root: &CheckedWorkspaceRoot) -> Result<(), String> {
    let metadata = root.file.metadata().map_err(|error| {
        format!(
            "cannot recheck session workspace root '{}': {error}",
            root.requested.display()
        )
    })?;
    if !metadata.is_dir() || metadata_is_link_or_reparse(&metadata) {
        return Err(format!(
            "session workspace root '{}' is no longer a checked directory",
            root.requested.display()
        ));
    }
    let actual = opened_file_path(&root.file, &root.requested).map_err(|error| {
        format!(
            "cannot recheck session workspace root '{}': {error}",
            root.requested.display()
        )
    })?;
    if !paths_are_same(&actual, &root.requested) {
        return Err(format!(
            "session workspace root '{}' changed while its trust anchor was open",
            root.requested.display()
        ));
    }
    Ok(())
}

fn verify_checked_workspace_capability(path: &CheckedWorkspacePath) -> Result<(), String> {
    verify_checked_workspace_root(&path.root)?;
    let namespace_metadata = path.namespace.file.metadata().map_err(|error| {
        format!(
            "cannot recheck workspace namespace for '{}': {error}",
            path.requested.display()
        )
    })?;
    if !namespace_metadata.is_dir() || metadata_is_link_or_reparse(&namespace_metadata) {
        return Err(format!(
            "workspace namespace for '{}' is no longer a checked directory",
            path.requested.display()
        ));
    }
    let expected_namespace = path.root.requested.join(&path.namespace.relative);
    let actual_namespace =
        opened_file_path(&path.namespace.file, &expected_namespace).map_err(|error| {
            format!(
                "cannot recheck workspace namespace for '{}': {error}",
                path.requested.display()
            )
        })?;
    if !paths_are_same(&actual_namespace, &expected_namespace) {
        return Err(format!(
            "workspace namespace '{}' changed after it was resolved",
            expected_namespace.display()
        ));
    }

    if let Some(probe) = path.probe.as_deref() {
        #[cfg(not(windows))]
        {
            let metadata = probe.metadata().map_err(|error| {
                format!(
                    "cannot recheck workspace target '{}': {error}",
                    path.requested.display()
                )
            })?;
            if metadata_is_link_or_reparse(&metadata) {
                return Err(format!(
                    "workspace target '{}' became a link or reparse point",
                    path.requested.display()
                ));
            }
        }
        #[cfg(windows)]
        {
            if path.suffix.as_os_str().is_empty() {
                if !same_opened_file_identity(probe, &path.namespace.file).map_err(|error| {
                    format!(
                        "cannot compare workspace root target '{}': {error}",
                        path.requested.display()
                    )
                })? {
                    return Err(format!(
                        "workspace root target '{}' changed after it was resolved",
                        path.requested.display()
                    ));
                }
                return Ok(());
            }
            let component = path.suffix.file_name().ok_or_else(|| {
                format!(
                    "workspace target '{}' has no checked final component",
                    path.requested.display()
                )
            })?;
            let current =
                probe_workspace_target_component(&path.namespace.file, component, &path.requested)
                    .map_err(|error| {
                        format!(
                            "cannot recheck workspace target '{}': {error}",
                            path.requested.display()
                        )
                    })?
                    .ok_or_else(|| {
                        format!(
                            "workspace target '{}' disappeared after it was resolved",
                            path.requested.display()
                        )
                    })?;
            if !same_opened_file_identity(probe, &current).map_err(|error| {
                format!(
                    "cannot compare workspace target '{}': {error}",
                    path.requested.display()
                )
            })? {
                return Err(format!(
                    "workspace target '{}' changed after it was resolved",
                    path.requested.display()
                ));
            }
        }
        #[cfg(not(windows))]
        {
            let actual = opened_file_path(probe, &path.requested).map_err(|error| {
                format!(
                    "cannot recheck workspace target '{}': {error}",
                    path.requested.display()
                )
            })?;
            if !paths_are_same(&actual, &path.requested) {
                return Err(format!(
                    "workspace target '{}' changed after it was resolved",
                    path.requested.display()
                ));
            }
        }
    }
    Ok(())
}

fn bind_workspace_namespace_with_probe_hook(
    root: &Arc<CheckedWorkspaceRoot>,
    relative: &Path,
    requested: &Path,
    probe_hook: &mut dyn FnMut(),
) -> Result<WorkspaceNamespaceBinding, String> {
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(part) => Ok(part.to_os_string()),
            _ => Err("workspace path is not normalized".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut directory = root
        .file
        .try_clone()
        .map_err(|error| checked_relative_open_error(&root.requested, error))?;
    let mut directory_relative = PathBuf::new();
    if components.is_empty() {
        let probe = directory
            .try_clone()
            .map(Arc::new)
            .map_err(|error| checked_relative_open_error(requested, error))?;
        return Ok(WorkspaceNamespaceBinding {
            namespace: Arc::new(CheckedWorkspaceNamespace {
                file: directory,
                relative: directory_relative,
            }),
            suffix: PathBuf::new(),
            probe: Some(probe),
        });
    }

    for (index, component) in components.iter().enumerate() {
        let last = index + 1 == components.len();
        if last {
            probe_hook();
            let probe = probe_workspace_target_component(&directory, component, requested)
                .map_err(|error| checked_relative_open_error(requested, error))?
                .map(Arc::new);
            return Ok(WorkspaceNamespaceBinding {
                namespace: Arc::new(CheckedWorkspaceNamespace {
                    file: directory,
                    relative: directory_relative,
                }),
                suffix: PathBuf::from(component),
                probe,
            });
        }
        let opened = open_workspace_namespace_component(&directory, component, true);
        let opened = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let suffix = components[index..].iter().collect::<PathBuf>();
                return Ok(WorkspaceNamespaceBinding {
                    namespace: Arc::new(CheckedWorkspaceNamespace {
                        file: directory,
                        relative: directory_relative,
                    }),
                    suffix,
                    probe: None,
                });
            }
            Err(error) => return Err(checked_relative_open_error(requested, error)),
        };
        let metadata = opened
            .metadata()
            .map_err(|error| checked_relative_open_error(requested, error))?;
        if metadata_is_link_or_reparse(&metadata) {
            return Err(format!(
                "path '{}' crosses a link or reparse point",
                requested.display()
            ));
        }
        if !metadata.is_dir() {
            return Err(format!(
                "path '{}' crosses a non-directory component",
                requested.display()
            ));
        }
        directory = opened;
        directory_relative.push(component);
    }
    Err("workspace namespace binding ended unexpectedly".to_string())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn probe_workspace_target_component(
    parent: &File,
    component: &OsStr,
    _: &Path,
) -> std::io::Result<Option<File>> {
    match open_relative_from_file(
        parent,
        Path::new(component),
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    ) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(windows)]
fn probe_workspace_target_component(
    parent: &File,
    component: &OsStr,
    requested: &Path,
) -> std::io::Result<Option<File>> {
    use windows_sys::{
        Wdk::Storage::FileSystem::{
            FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
        },
        Win32::{
            Foundation::{
                ERROR_ACCESS_DENIED, ERROR_DIRECTORY, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND,
            },
            Storage::FileSystem::{
                FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
            },
        },
    };

    let open_probe = |options| {
        open_windows_relative_component_no_follow(
            parent,
            component,
            SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | options,
        )
    };
    let opened = match open_probe(FILE_NON_DIRECTORY_FILE) {
        Ok(file) => Ok(file),
        Err(error)
            if error.raw_os_error().is_some_and(|code| {
                code == ERROR_DIRECTORY as i32 || code == ERROR_ACCESS_DENIED as i32
            }) =>
        {
            // A directory opened without FILE_DIRECTORY_FILE commonly reports
            // ERROR_ACCESS_DENIED. Retry only as a directory; if that also
            // fails, preserve the original access error instead of treating a
            // protected regular file as absent.
            open_probe(FILE_DIRECTORY_FILE).map_err(|directory_error| {
                if directory_error.raw_os_error() == Some(ERROR_DIRECTORY as i32) {
                    error
                } else {
                    directory_error
                }
            })
        }
        Err(error) => Err(error),
    };
    match opened {
        Ok(file) => Ok(Some(file)),
        Err(error)
            if error.raw_os_error().is_some_and(|code| {
                code == ERROR_FILE_NOT_FOUND as i32 || code == ERROR_PATH_NOT_FOUND as i32
            }) =>
        {
            // Only an actually absent target may omit the final identity
            // handle. The probe requests only SYNCHRONIZE (no file or
            // directory data access), preserving write-only compatibility
            // while still distinguishing regular files from absence.
            Ok(None)
        }
        Err(error) => Err(std::io::Error::new(
            error.kind(),
            format!(
                "cannot probe final workspace component '{}' for '{}': {error}",
                component.to_string_lossy(),
                requested.display()
            ),
        )),
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn probe_workspace_target_component(
    _: &File,
    _: &OsStr,
    _: &Path,
) -> std::io::Result<Option<File>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure mount-bound workspace capabilities are unavailable on this Unix platform",
    ))
}

#[cfg(not(any(unix, windows)))]
fn probe_workspace_target_component(
    _: &File,
    _: &OsStr,
    _: &Path,
) -> std::io::Result<Option<File>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure workspace capabilities are unavailable on this platform",
    ))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_workspace_namespace_component(
    parent: &File,
    component: &OsStr,
    directory: bool,
) -> std::io::Result<File> {
    open_relative_from_file(
        parent,
        Path::new(component),
        libc::O_PATH
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if directory { libc::O_DIRECTORY } else { 0 },
        0,
    )
}

#[cfg(windows)]
fn open_workspace_namespace_component(
    parent: &File,
    component: &OsStr,
    directory: bool,
) -> std::io::Result<File> {
    use windows_sys::{
        Wdk::Storage::FileSystem::{FILE_DIRECTORY_FILE, FILE_OPEN},
        Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
        },
    };

    let file = open_windows_relative_component(
        parent,
        component,
        if directory {
            FILE_TRAVERSE | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES
        } else {
            0
        },
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        FILE_OPEN,
        if directory { FILE_DIRECTORY_FILE } else { 0 },
    )?;
    reject_windows_reparse_handle(&file)?;
    Ok(file)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn open_workspace_namespace_component(_: &File, _: &OsStr, _: bool) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure mount-bound workspace capabilities are unavailable on this Unix platform",
    ))
}

#[cfg(not(any(unix, windows)))]
fn open_workspace_namespace_component(_: &File, _: &OsStr, _: bool) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure workspace capabilities are unavailable on this platform",
    ))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn reopen_workspace_directory_namespace(file: &File) -> std::io::Result<File> {
    file.try_clone()
}

#[cfg(windows)]
fn reopen_workspace_directory_namespace(file: &File) -> std::io::Result<File> {
    let directory = file.try_clone()?;
    reject_windows_reparse_handle(&directory)?;
    Ok(directory)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn reopen_workspace_directory_namespace(_: &File) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure mount-bound workspace capabilities are unavailable on this Unix platform",
    ))
}

#[cfg(not(any(unix, windows)))]
fn reopen_workspace_directory_namespace(_: &File) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure workspace capabilities are unavailable on this platform",
    ))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[repr(C)]
struct WorkspaceOpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

#[cfg(any(target_os = "linux", target_os = "android"))]
const WORKSPACE_RESOLVE_NO_XDEV: u64 = 0x01;
#[cfg(any(target_os = "linux", target_os = "android"))]
const WORKSPACE_RESOLVE_NO_MAGICLINKS: u64 = 0x02;
#[cfg(any(target_os = "linux", target_os = "android"))]
const WORKSPACE_RESOLVE_NO_SYMLINKS: u64 = 0x04;
#[cfg(any(target_os = "linux", target_os = "android"))]
const WORKSPACE_RESOLVE_BENEATH: u64 = 0x08;

#[cfg(any(target_os = "linux", target_os = "android"))]
fn workspace_open_how(flags: i32, mode: u32) -> WorkspaceOpenHow {
    // Keep the published Linux openat2 ABI local because libc exposes
    // `open_how` on Linux targets but not on every Android target it supports.
    WorkspaceOpenHow {
        flags: flags as u64,
        mode: mode as u64,
        resolve: WORKSPACE_RESOLVE_BENEATH
            | WORKSPACE_RESOLVE_NO_SYMLINKS
            | WORKSPACE_RESOLVE_NO_MAGICLINKS
            | WORKSPACE_RESOLVE_NO_XDEV,
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_relative_from_file(
    parent: &File,
    relative: &Path,
    flags: i32,
    mode: u32,
) -> std::io::Result<File> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt as _, os::unix::io::FromRawFd as _};

    let relative = CString::new(relative.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "workspace path contains a NUL byte",
        )
    })?;
    let how = workspace_open_how(flags, mode);
    use std::os::fd::AsRawFd as _;
    // SAFETY: `root` owns a live directory fd, both pointers remain valid for
    // the syscall, and `how` has the kernel's published `open_how` layout.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent.as_raw_fd(),
            relative.as_ptr(),
            &how,
            std::mem::size_of::<WorkspaceOpenHow>(),
        )
    };
    if fd == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a successful openat2 returns one newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_relative_from_namespace(path: &CheckedWorkspacePath) -> std::io::Result<File> {
    open_relative_from_file(
        &path.namespace.file,
        &path.suffix,
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn open_relative_from_namespace(_: &CheckedWorkspacePath) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure mount-bound workspace capabilities are unavailable on this Unix platform",
    ))
}

#[cfg(windows)]
fn open_windows_relative_component(
    parent: &File,
    component: &OsStr,
    desired_access: u32,
    share_mode: u32,
    disposition: u32,
    create_options: u32,
) -> std::io::Result<File> {
    open_windows_relative_component_impl(
        parent,
        component,
        desired_access,
        share_mode,
        disposition,
        create_options,
        true,
    )
}

#[cfg(windows)]
fn open_windows_relative_component_no_follow(
    parent: &File,
    component: &OsStr,
    desired_access: u32,
    share_mode: u32,
    disposition: u32,
    create_options: u32,
) -> std::io::Result<File> {
    open_windows_relative_component_impl(
        parent,
        component,
        desired_access,
        share_mode,
        disposition,
        create_options,
        false,
    )
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn open_windows_relative_component_impl(
    parent: &File,
    component: &OsStr,
    desired_access: u32,
    share_mode: u32,
    disposition: u32,
    create_options: u32,
    open_reparse_point: bool,
) -> std::io::Result<File> {
    use std::os::windows::io::AsRawHandle as _;
    use std::os::{windows::ffi::OsStrExt as _, windows::io::FromRawHandle as _};
    use windows_sys::{
        Wdk::{
            Foundation::OBJECT_ATTRIBUTES,
            Storage::FileSystem::{
                FILE_OPEN_FOR_BACKUP_INTENT, FILE_OPEN_REPARSE_POINT, NtCreateFile,
            },
        },
        Win32::{
            Foundation::{
                HANDLE, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, RtlNtStatusToDosError,
                UNICODE_STRING,
            },
            Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
            System::IO::IO_STATUS_BLOCK,
        },
    };

    let mut name = component.encode_wide().collect::<Vec<_>>();
    let byte_len = name
        .len()
        .checked_mul(std::mem::size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace path component is too long",
            )
        })?;
    let unicode = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &unicode,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut io_status = IO_STATUS_BLOCK::default();
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: all structures and the UTF-16 component outlive the synchronous
    // call; the parent handle is live and successful return transfers one
    // owned child handle to this function.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &attributes,
            &mut io_status,
            std::ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            share_mode,
            disposition,
            (if desired_access == 0 {
                0
            } else {
                FILE_OPEN_FOR_BACKUP_INTENT
            }) | if open_reparse_point {
                FILE_OPEN_REPARSE_POINT
            } else {
                0
            } | create_options,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: conversion accepts any NTSTATUS returned by NtCreateFile.
        let error = unsafe { RtlNtStatusToDosError(status) };
        return Err(std::io::Error::from_raw_os_error(error as i32));
    }
    if handle.is_null() {
        return Err(std::io::Error::other(
            "relative workspace open returned no handle",
        ));
    }
    // SAFETY: successful NtCreateFile returned one newly owned handle.
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(windows)]
fn reject_windows_reparse_handle(file: &File) -> std::io::Result<()> {
    let metadata = file.metadata()?;
    if metadata_is_link_or_reparse(&metadata) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "workspace path crosses a reparse point",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn open_relative_from_namespace(path: &CheckedWorkspacePath) -> std::io::Result<File> {
    use windows_sys::{
        Wdk::Storage::FileSystem::{FILE_DIRECTORY_FILE, FILE_OPEN},
        Win32::Storage::FileSystem::{
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            FILE_TRAVERSE,
        },
    };

    let components = path.suffix.components().collect::<Vec<_>>();
    let mut parent = path.namespace.file.try_clone()?;
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(part) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "workspace path is not normalized",
            ));
        };
        let last = index + 1 == components.len();
        parent = open_windows_relative_component(
            &parent,
            part,
            if last {
                FILE_READ_ATTRIBUTES
            } else {
                FILE_TRAVERSE | FILE_READ_ATTRIBUTES
            },
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_OPEN,
            if last { 0 } else { FILE_DIRECTORY_FILE },
        )?;
        if !last {
            reject_windows_reparse_handle(&parent)?;
        }
    }
    Ok(parent)
}

#[cfg(not(any(unix, windows)))]
fn open_relative_from_namespace(_: &CheckedWorkspacePath) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure handle-relative workspace resolution is unavailable on this platform",
    ))
}

fn checked_relative_open_error(requested: &Path, error: std::io::Error) -> String {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if error
        .raw_os_error()
        .is_some_and(|code| code == libc::ENOSYS || code == libc::EINVAL)
    {
        return format!(
            "cannot open '{}': secure openat2 workspace resolution is unavailable; refusing an unsafe fallback",
            requested.display()
        );
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if error.raw_os_error() == Some(libc::EXDEV) {
        return format!(
            "path '{}' crosses a workspace mount boundary",
            requested.display()
        );
    }
    format!(
        "cannot open '{}' relative to its checked workspace root without following links or mounts: {error}",
        requested.display()
    )
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn reopen_proc_fd(file: &File, flags: i32) -> std::io::Result<File> {
    use std::{
        ffi::CString,
        os::fd::{AsRawFd as _, FromRawFd as _},
    };

    let path = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|_| std::io::Error::other("invalid proc-fd path"))?;
    // SAFETY: path is a NUL-terminated procfs reference to the retained fd;
    // successful open returns one independent operation handle.
    let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_CLOEXEC) };
    if fd == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: successful open returned one newly owned descriptor.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn validate_operation_anchors(anchors: &[WorkspaceIdentityAnchor]) -> Result<(), String> {
    for anchor in anchors {
        let actual = opened_file_path(&anchor.file, &anchor.expected)
            .map_err(|error| checked_relative_open_error(&anchor.expected, error))?;
        if !paths_are_same(&actual, &anchor.expected) {
            return Err(format!(
                "workspace operation anchor '{}' moved after it was opened",
                anchor.expected.display()
            ));
        }
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn fresh_linux_workspace_namespace(
    path: &CheckedWorkspacePath,
) -> Result<(File, Vec<WorkspaceIdentityAnchor>), String> {
    let fresh_root = open_checked_workspace_root(&path.root.requested)?;
    if !same_opened_file_identity(&fresh_root.file, &path.root.file)
        .map_err(|error| checked_relative_open_error(&path.root.requested, error))?
    {
        return Err(format!(
            "session workspace root '{}' changed after it was resolved",
            path.root.requested.display()
        ));
    }
    let mut anchors = vec![WorkspaceIdentityAnchor {
        file: fresh_root
            .file
            .try_clone()
            .map_err(|error| checked_relative_open_error(&path.root.requested, error))?,
        expected: path.root.requested.clone(),
    }];
    let namespace = if path.namespace.relative.as_os_str().is_empty() {
        fresh_root.file
    } else {
        open_relative_from_file(
            &fresh_root.file,
            &path.namespace.relative,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        )
        .map_err(|error| checked_relative_open_error(&path.requested, error))?
    };
    if !same_opened_file_identity(&namespace, &path.namespace.file)
        .map_err(|error| checked_relative_open_error(&path.requested, error))?
    {
        return Err(format!(
            "workspace namespace for '{}' changed after it was resolved",
            path.requested.display()
        ));
    }
    anchors.push(WorkspaceIdentityAnchor {
        file: namespace
            .try_clone()
            .map_err(|error| checked_relative_open_error(&path.requested, error))?,
        expected: path.root.requested.join(&path.namespace.relative),
    });
    validate_operation_anchors(&anchors)?;
    Ok((namespace, anchors))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn fresh_linux_existing_target(
    path: &CheckedWorkspacePath,
    flags: i32,
) -> Result<CheckedWorkspaceFile, String> {
    let probe = path.probe.as_deref().ok_or_else(|| {
        format!(
            "workspace target '{}' did not exist when it was resolved",
            path.requested.display()
        )
    })?;
    let (namespace, mut anchors) = fresh_linux_workspace_namespace(path)?;
    let target = if path.suffix.as_os_str().is_empty() {
        namespace
            .try_clone()
            .map_err(|error| checked_relative_open_error(&path.requested, error))?
    } else {
        open_relative_from_file(
            &namespace,
            &path.suffix,
            libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        )
        .map_err(|error| checked_relative_open_error(&path.requested, error))?
    };
    if !same_opened_file_identity(&target, probe)
        .map_err(|error| checked_relative_open_error(&path.requested, error))?
    {
        return Err(format!(
            "workspace target '{}' changed after it was resolved",
            path.requested.display()
        ));
    }
    anchors.push(WorkspaceIdentityAnchor {
        file: target
            .try_clone()
            .map_err(|error| checked_relative_open_error(&path.requested, error))?,
        expected: path.requested.clone(),
    });
    validate_operation_anchors(&anchors)?;
    let file = reopen_proc_fd(&target, flags)
        .map_err(|error| checked_relative_open_error(&path.requested, error))?;
    CheckedWorkspaceFile::new(path, file, anchors)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_workspace_file_for_read(
    path: &CheckedWorkspacePath,
) -> Result<CheckedWorkspaceFile, String> {
    fresh_linux_existing_target(path, libc::O_RDONLY)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_or_create_workspace_parent(
    path: &CheckedWorkspacePath,
) -> Result<(File, std::ffi::OsString, Vec<WorkspaceIdentityAnchor>), String> {
    use std::{ffi::CString, os::fd::AsRawFd as _, os::unix::ffi::OsStrExt as _};

    let name = path
        .suffix
        .file_name()
        .ok_or_else(|| format!("'{}' is not a file path", path.requested.display()))?
        .to_os_string();
    let (mut parent, mut anchors) = fresh_linux_workspace_namespace(path)?;
    let parent_relative = path.suffix.parent().unwrap_or_else(|| Path::new(""));
    let mut expected_parent = path.root.requested.join(&path.namespace.relative);
    for component in parent_relative.components() {
        let Component::Normal(part) = component else {
            return Err("workspace path is not normalized".to_string());
        };
        let component_path = Path::new(part);
        let opened = open_relative_from_file(
            &parent,
            component_path,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        );
        parent = match opened {
            Ok(_) => {
                return Err(format!(
                    "workspace path '{}' changed after it was resolved",
                    path.requested.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                validate_operation_anchors(&anchors)?;
                let part = CString::new(part.as_bytes()).map_err(|_| {
                    "workspace path contains a NUL byte and cannot be created".to_string()
                })?;
                // SAFETY: parent is a live directory fd and part is one
                // normalized, NUL-terminated component.
                let result = unsafe { libc::mkdirat(parent.as_raw_fd(), part.as_ptr(), 0o777) };
                if result == -1 {
                    let mkdir_error = std::io::Error::last_os_error();
                    if mkdir_error.kind() != std::io::ErrorKind::AlreadyExists {
                        return Err(checked_relative_open_error(&path.requested, mkdir_error));
                    }
                }
                open_relative_from_file(
                    &parent,
                    component_path,
                    libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                    0,
                )
                .map_err(|error| checked_relative_open_error(&path.requested, error))?
            }
            Err(error) => {
                return Err(checked_relative_open_error(&path.requested, error));
            }
        };
        expected_parent.push(part);
        anchors.push(WorkspaceIdentityAnchor {
            file: parent
                .try_clone()
                .map_err(|error| checked_relative_open_error(&path.requested, error))?,
            expected: expected_parent.clone(),
        });
        validate_operation_anchors(&anchors)?;
    }
    Ok((parent, name, anchors))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_workspace_file_for_write(
    path: &CheckedWorkspacePath,
    truncate: bool,
    hook: &mut dyn FnMut(),
) -> Result<CheckedWorkspaceFile, String> {
    if path.probe.is_some() {
        let file = fresh_linux_existing_target(path, libc::O_WRONLY)?;
        hook();
        if truncate {
            file.set_len(0).map_err(|error| error.to_string())?;
        }
        return Ok(file);
    }
    let (parent, name, mut anchors) = open_or_create_workspace_parent(path)?;
    validate_operation_anchors(&anchors)?;
    hook();
    validate_operation_anchors(&anchors)?;
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    let file = open_relative_from_file(&parent, Path::new(&name), flags, 0o666)
        .map_err(|error| checked_relative_open_error(&path.requested, error))?;
    anchors.push(WorkspaceIdentityAnchor {
        file: file
            .try_clone()
            .map_err(|error| checked_relative_open_error(&path.requested, error))?,
        expected: path.requested.clone(),
    });
    CheckedWorkspaceFile::new(path, file, anchors)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_workspace_file_for_patch(
    path: &CheckedWorkspacePath,
) -> Result<CheckedWorkspaceFile, String> {
    fresh_linux_existing_target(path, libc::O_RDWR)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn remove_workspace_file(
    path: &CheckedWorkspacePath,
    hook: &mut dyn FnMut(),
) -> Result<(), String> {
    use std::{ffi::CString, os::fd::AsRawFd as _, os::unix::ffi::OsStrExt as _};

    let probe = path.probe.as_deref().ok_or_else(|| {
        format!(
            "workspace target '{}' did not exist when it was resolved",
            path.requested.display()
        )
    })?;
    let name = path
        .relative
        .file_name()
        .ok_or_else(|| "the workspace root cannot be deleted".to_string())?;
    let parent_relative = path.relative.parent().unwrap_or_else(|| Path::new(""));
    let fresh_root = open_checked_workspace_root(&path.root.requested)?;
    if !same_opened_file_identity(&fresh_root.file, &path.root.file)
        .map_err(|error| checked_relative_open_error(&path.root.requested, error))?
    {
        return Err(format!(
            "session workspace root '{}' changed after it was resolved",
            path.root.requested.display()
        ));
    }
    let parent = if parent_relative.as_os_str().is_empty() {
        fresh_root.file.try_clone()
    } else {
        open_relative_from_file(
            &fresh_root.file,
            parent_relative,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        )
    }
    .map_err(|error| checked_relative_open_error(&path.requested, error))?;
    let current = open_relative_from_file(
        &parent,
        Path::new(name),
        libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        0,
    )
    .map_err(|error| checked_relative_open_error(&path.requested, error))?;
    if !same_opened_file_identity(&current, probe)
        .map_err(|error| checked_relative_open_error(&path.requested, error))?
    {
        return Err(format!(
            "workspace target '{}' changed after it was resolved",
            path.requested.display()
        ));
    }
    let anchors = vec![
        WorkspaceIdentityAnchor {
            file: fresh_root.file,
            expected: path.root.requested.clone(),
        },
        WorkspaceIdentityAnchor {
            file: parent,
            expected: path.root.requested.join(parent_relative),
        },
        WorkspaceIdentityAnchor {
            file: current,
            expected: path.requested.clone(),
        },
    ];
    validate_operation_anchors(&anchors)?;
    hook();
    validate_operation_anchors(&anchors)?;
    let name = CString::new(name.as_bytes())
        .map_err(|_| "workspace path contains a NUL byte".to_string())?;
    // SAFETY: parent is the checked directory capability and name is one
    // normalized component. unlinkat with flags=0 cannot remove a directory.
    let result = unsafe { libc::unlinkat(anchors[1].file.as_raw_fd(), name.as_ptr(), 0) };
    if result == -1 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok(())
}

#[cfg(windows)]
fn open_windows_path_for_operation(
    path: &CheckedWorkspacePath,
    final_access: u32,
    final_disposition: u32,
    final_options: u32,
    share_delete: bool,
) -> Result<CheckedWorkspaceFile, String> {
    use windows_sys::{
        Wdk::Storage::FileSystem::{FILE_DIRECTORY_FILE, FILE_OPEN},
        Win32::Storage::FileSystem::{
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
        },
    };

    let probe = path.probe.as_deref().ok_or_else(|| {
        format!(
            "workspace target '{}' did not exist when it was resolved",
            path.requested.display()
        )
    })?;
    if final_disposition != FILE_OPEN {
        return Err("existing workspace operation requires FILE_OPEN".to_string());
    }
    let components = path.relative.components().collect::<Vec<_>>();
    if components.is_empty() {
        return Err(format!("'{}' is not a file", path.requested.display()));
    }
    let root_lock = path
        .root
        .file
        .try_clone()
        .map_err(|error| checked_relative_open_error(&path.root.requested, error))?;
    let mut anchors = vec![WorkspaceIdentityAnchor {
        file: root_lock,
        expected: path.root.requested.clone(),
    }];
    let mut expected = path.root.requested.clone();
    let namespace_depth = path.namespace.relative.components().count();
    if namespace_depth == 0
        && !same_opened_file_identity(&anchors[0].file, &path.namespace.file)
            .map_err(|error| checked_relative_open_error(&path.requested, error))?
    {
        return Err(format!(
            "workspace namespace for '{}' changed after it was resolved",
            path.requested.display()
        ));
    }
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(part) = component else {
            return Err("workspace path is not normalized".to_string());
        };
        expected.push(part);
        let last = index + 1 == components.len();
        let parent = &anchors
            .last()
            .ok_or_else(|| "workspace operation lock chain was lost".to_string())?
            .file;
        let opened = if last {
            open_windows_relative_component(
                parent,
                part,
                final_access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                FILE_OPEN,
                final_options,
            )
        } else {
            open_windows_relative_component(
                parent,
                part,
                FILE_TRAVERSE | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                FILE_OPEN,
                FILE_DIRECTORY_FILE,
            )
        }
        .or_else(|error| {
            if last
                && share_delete
                && error.raw_os_error()
                    == Some(windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32)
            {
                open_windows_relative_component(
                    parent,
                    part,
                    final_access,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    FILE_OPEN,
                    final_options,
                )
            } else {
                Err(error)
            }
        })
        .map_err(|error| {
            format!(
                "cannot {} workspace operation component '{}' for '{}': {error}",
                if last { "open final" } else { "lock" },
                part.to_string_lossy(),
                path.requested.display()
            )
        })?;
        if !last {
            reject_windows_reparse_handle(&opened)
                .map_err(|error| checked_relative_open_error(&path.requested, error))?;
        }
        if index + 1 == namespace_depth
            && !same_opened_file_identity(&opened, &path.namespace.file)
                .map_err(|error| checked_relative_open_error(&path.requested, error))?
        {
            return Err(format!(
                "workspace namespace for '{}' changed after it was resolved",
                path.requested.display()
            ));
        }
        if last
            && !same_opened_file_identity(&opened, probe)
                .map_err(|error| checked_relative_open_error(&path.requested, error))?
        {
            return Err(format!(
                "workspace target '{}' changed after it was resolved",
                path.requested.display()
            ));
        }
        anchors.push(WorkspaceIdentityAnchor {
            file: opened,
            expected: expected.clone(),
        });
    }
    let target = anchors
        .pop()
        .ok_or_else(|| "workspace target operation handle was lost".to_string())?;
    CheckedWorkspaceFile::new(path, target.file, anchors)
}

#[cfg(windows)]
fn open_or_create_windows_parent(
    path: &CheckedWorkspacePath,
) -> Result<(File, std::ffi::OsString, Vec<WorkspaceIdentityAnchor>), String> {
    use windows_sys::{
        Wdk::Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_OPEN, FILE_SYNCHRONOUS_IO_NONALERT,
        },
        Win32::Storage::FileSystem::{
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, SYNCHRONIZE,
        },
    };

    let name = path
        .suffix
        .file_name()
        .ok_or_else(|| format!("'{}' is not a file path", path.requested.display()))?
        .to_os_string();
    let root_lock = path
        .root
        .file
        .try_clone()
        .map_err(|error| checked_relative_open_error(&path.root.requested, error))?;
    let mut anchors = vec![WorkspaceIdentityAnchor {
        file: root_lock,
        expected: path.root.requested.clone(),
    }];
    let namespace_depth = path.namespace.relative.components().count();
    if namespace_depth == 0
        && !same_opened_file_identity(&anchors[0].file, &path.namespace.file)
            .map_err(|error| checked_relative_open_error(&path.requested, error))?
    {
        return Err(format!(
            "workspace namespace for '{}' changed after it was resolved",
            path.requested.display()
        ));
    }
    let parent_relative = path.relative.parent().unwrap_or_else(|| Path::new(""));
    let mut expected = path.root.requested.clone();
    for (index, component) in parent_relative.components().enumerate() {
        let Component::Normal(part) = component else {
            return Err("workspace path is not normalized".to_string());
        };
        expected.push(part);
        let parent = &anchors
            .last()
            .ok_or_else(|| "workspace operation lock chain was lost".to_string())?
            .file;
        let within_retained_namespace = index < namespace_depth;
        let child = open_windows_relative_component(
            parent,
            part,
            FILE_TRAVERSE
                | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES
                | SYNCHRONIZE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            if within_retained_namespace {
                FILE_OPEN
            } else {
                FILE_CREATE
            },
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
        )
        .map_err(|error| {
            format!(
                "cannot {} workspace directory component '{}' for '{}': {error}",
                if within_retained_namespace {
                    "lock"
                } else {
                    "create"
                },
                part.to_string_lossy(),
                path.requested.display()
            )
        })?;
        reject_windows_reparse_handle(&child)
            .map_err(|error| checked_relative_open_error(&path.requested, error))?;
        if index + 1 == namespace_depth
            && !same_opened_file_identity(&child, &path.namespace.file)
                .map_err(|error| checked_relative_open_error(&path.requested, error))?
        {
            return Err(format!(
                "workspace namespace for '{}' changed after it was resolved",
                path.requested.display()
            ));
        }
        anchors.push(WorkspaceIdentityAnchor {
            file: child,
            expected: expected.clone(),
        });
    }
    let parent = anchors
        .last()
        .ok_or_else(|| "workspace operation parent lock was lost".to_string())?
        .file
        .try_clone()
        .map_err(|error| checked_relative_open_error(&path.requested, error))?;
    Ok((parent, name, anchors))
}

#[cfg(windows)]
fn open_workspace_file_for_read(
    path: &CheckedWorkspacePath,
) -> Result<CheckedWorkspaceFile, String> {
    use windows_sys::{
        Wdk::Storage::FileSystem::{
            FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_SYNCHRONOUS_IO_NONALERT,
        },
        Win32::Storage::FileSystem::{FILE_READ_ATTRIBUTES, FILE_READ_DATA, SYNCHRONIZE},
    };

    verify_checked_workspace_root(&path.root)?;
    open_windows_path_for_operation(
        path,
        FILE_READ_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
        true,
    )
}

#[cfg(windows)]
fn open_workspace_file_for_write(
    path: &CheckedWorkspacePath,
    truncate: bool,
    hook: &mut dyn FnMut(),
) -> Result<CheckedWorkspaceFile, String> {
    use windows_sys::{
        Wdk::Storage::FileSystem::{
            FILE_CREATE, FILE_NON_DIRECTORY_FILE, FILE_SYNCHRONOUS_IO_NONALERT,
        },
        Win32::Storage::FileSystem::{FILE_WRITE_DATA, SYNCHRONIZE},
    };

    verify_checked_workspace_root(&path.root)?;
    if path.probe.is_some() {
        let file = open_windows_path_for_operation(
            path,
            FILE_WRITE_DATA | SYNCHRONIZE,
            windows_sys::Wdk::Storage::FileSystem::FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
            true,
        )?;
        hook();
        file.validate().map_err(|error| error.to_string())?;
        if truncate {
            file.set_len(0).map_err(|error| error.to_string())?;
        }
        return Ok(file);
    }
    let (parent, name, anchors) = open_or_create_windows_parent(path)?;
    validate_operation_anchors(&anchors)?;
    hook();
    validate_operation_anchors(&anchors)?;
    let file = open_windows_relative_component(
        &parent,
        &name,
        FILE_WRITE_DATA | SYNCHRONIZE,
        windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
            | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE,
        FILE_CREATE,
        FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
    )
    .map_err(|error| {
        format!(
            "cannot create workspace file '{}': {error}",
            path.requested.display()
        )
    })?;
    CheckedWorkspaceFile::new(path, file, anchors)
}

#[cfg(windows)]
fn open_workspace_file_for_patch(
    path: &CheckedWorkspacePath,
) -> Result<CheckedWorkspaceFile, String> {
    use windows_sys::{
        Wdk::Storage::FileSystem::{
            FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_SYNCHRONOUS_IO_NONALERT,
        },
        Win32::Storage::FileSystem::{FILE_READ_DATA, FILE_WRITE_DATA, SYNCHRONIZE},
    };

    verify_checked_workspace_root(&path.root)?;
    open_windows_path_for_operation(
        path,
        FILE_READ_DATA | FILE_WRITE_DATA | SYNCHRONIZE,
        FILE_OPEN,
        FILE_NON_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT,
        true,
    )
}

#[cfg(windows)]
fn remove_workspace_file(
    path: &CheckedWorkspacePath,
    hook: &mut dyn FnMut(),
) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::{
        Wdk::Storage::FileSystem::FILE_NON_DIRECTORY_FILE,
        Win32::Storage::FileSystem::{
            DELETE, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
            FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FileDispositionInfo,
            FileDispositionInfoEx, SetFileInformationByHandle,
        },
    };

    verify_checked_workspace_root(&path.root)?;
    let file = open_windows_path_for_operation(
        path,
        DELETE,
        windows_sys::Wdk::Storage::FileSystem::FILE_OPEN,
        FILE_NON_DIRECTORY_FILE,
        true,
    )?;
    file.validate().map_err(|error| error.to_string())?;
    hook();
    file.validate().map_err(|error| error.to_string())?;
    let disposition_ex = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: file is a live checked handle and disposition has the exact
    // layout required by FileDispositionInfoEx.
    let mut result = unsafe {
        SetFileInformationByHandle(
            file.file.as_raw_handle(),
            FileDispositionInfoEx,
            (&disposition_ex as *const FILE_DISPOSITION_INFO_EX).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    };
    if result == 0 {
        let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
        // SAFETY: same checked handle with the legacy disposition layout for
        // filesystems that do not implement POSIX unlink semantics.
        result = unsafe {
            SetFileInformationByHandle(
                file.file.as_raw_handle(),
                FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
            )
        };
    }
    if result == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    drop(file);
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn unsupported_unix_operation<T>() -> Result<T, String> {
    Err("secure mount-bound workspace operations are unavailable on this Unix platform".into())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn open_workspace_file_for_read(_: &CheckedWorkspacePath) -> Result<CheckedWorkspaceFile, String> {
    unsupported_unix_operation()
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn open_workspace_file_for_write(
    _: &CheckedWorkspacePath,
    _: bool,
    _: &mut dyn FnMut(),
) -> Result<CheckedWorkspaceFile, String> {
    unsupported_unix_operation()
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn open_workspace_file_for_patch(_: &CheckedWorkspacePath) -> Result<CheckedWorkspaceFile, String> {
    unsupported_unix_operation()
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn remove_workspace_file(_: &CheckedWorkspacePath, _: &mut dyn FnMut()) -> Result<(), String> {
    unsupported_unix_operation()
}

#[cfg(not(any(unix, windows)))]
fn open_workspace_file_for_read(_: &CheckedWorkspacePath) -> Result<CheckedWorkspaceFile, String> {
    Err("secure workspace operations are unavailable on this platform".into())
}

#[cfg(not(any(unix, windows)))]
fn open_workspace_file_for_write(
    _: &CheckedWorkspacePath,
    _: bool,
    _: &mut dyn FnMut(),
) -> Result<CheckedWorkspaceFile, String> {
    Err("secure workspace operations are unavailable on this platform".into())
}

#[cfg(not(any(unix, windows)))]
fn open_workspace_file_for_patch(_: &CheckedWorkspacePath) -> Result<CheckedWorkspaceFile, String> {
    Err("secure workspace operations are unavailable on this platform".into())
}

#[cfg(not(any(unix, windows)))]
fn remove_workspace_file(_: &CheckedWorkspacePath, _: &mut dyn FnMut()) -> Result<(), String> {
    Err("secure workspace operations are unavailable on this platform".into())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn checked_workspace_process_cwd(
    path: &CheckedWorkspacePath,
) -> Result<CheckedWorkspaceCwd, String> {
    use std::os::fd::AsRawFd as _;

    let probe = path.probe.as_deref().ok_or_else(|| {
        format!(
            "workspace process directory '{}' no longer exists",
            path.requested.display()
        )
    })?;
    let (namespace, mut anchors) = fresh_linux_workspace_namespace(path)?;
    let directory = if path.suffix.as_os_str().is_empty() {
        namespace
    } else {
        open_relative_from_file(
            &namespace,
            &path.suffix,
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0,
        )
        .map_err(|error| checked_relative_open_error(&path.requested, error))?
    };
    if !same_opened_file_identity(&directory, probe)
        .map_err(|error| checked_relative_open_error(&path.requested, error))?
    {
        return Err(format!(
            "workspace process directory '{}' changed after it was resolved",
            path.requested.display()
        ));
    }
    let directory_fd = directory.as_raw_fd();
    anchors.push(WorkspaceIdentityAnchor {
        file: directory,
        expected: path.requested.clone(),
    });
    validate_operation_anchors(&anchors)?;
    Ok(CheckedWorkspaceCwd {
        path: PathBuf::from(format!("/proc/self/fd/{directory_fd}")),
        source: path.clone(),
        anchors,
    })
}

#[cfg(windows)]
fn checked_workspace_process_cwd(
    path: &CheckedWorkspacePath,
) -> Result<CheckedWorkspaceCwd, String> {
    use windows_sys::{
        Wdk::Storage::FileSystem::{FILE_DIRECTORY_FILE, FILE_OPEN},
        Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE},
    };

    verify_checked_workspace_root(&path.root)?;
    // The persisted-root trust anchor was itself opened without delete
    // sharing. Cloning that exact handle preserves the namespace lock and does
    // not request extra rights that a traverse-only root ACL may deny.
    let root_lock = path
        .root
        .file
        .try_clone()
        .map_err(|error| checked_relative_open_error(&path.root.requested, error))?;
    let mut handles = vec![root_lock];
    for component in path.relative.components() {
        let Component::Normal(part) = component else {
            return Err("workspace path is not normalized".to_string());
        };
        let parent = handles
            .last()
            .ok_or_else(|| "workspace root lock was lost".to_string())?;
        let child = open_windows_relative_component(
            parent,
            part,
            FILE_TRAVERSE | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
        )
        .map_err(|error| checked_relative_open_error(&path.requested, error))?;
        reject_windows_reparse_handle(&child)
            .map_err(|error| checked_relative_open_error(&path.requested, error))?;
        handles.push(child);
    }
    let expected = path.probe.as_deref().ok_or_else(|| {
        format!(
            "workspace process directory '{}' no longer exists",
            path.requested.display()
        )
    })?;
    let actual = handles
        .last()
        .ok_or_else(|| "workspace directory lock chain was lost".to_string())?;
    if !same_opened_file_identity(actual, expected)
        .map_err(|error| checked_relative_open_error(&path.requested, error))?
    {
        return Err(format!(
            "workspace process directory '{}' changed after it was resolved",
            path.requested.display()
        ));
    }
    if !actual
        .metadata()
        .map_err(|error| checked_relative_open_error(&path.requested, error))?
        .is_dir()
    {
        return Err(format!("'{}' is not a directory", path.requested.display()));
    }
    verify_checked_workspace_root(&path.root)?;
    Ok(CheckedWorkspaceCwd {
        path: path.requested.clone(),
        source: path.clone(),
        anchors: handles
            .into_iter()
            .zip(std::iter::once(path.root.requested.clone()).chain(
                path.relative.components().scan(
                    path.root.requested.clone(),
                    |current, component| {
                        if let Component::Normal(part) = component {
                            current.push(part);
                            Some(current.clone())
                        } else {
                            None
                        }
                    },
                ),
            ))
            .map(|(file, expected)| WorkspaceIdentityAnchor { file, expected })
            .collect(),
    })
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowsFileIdentity {
    volume_serial_number: u64,
    file_id: [u8; 16],
}

#[cfg(windows)]
fn validate_windows_file_identity(
    identity: WindowsFileIdentity,
) -> std::io::Result<WindowsFileIdentity> {
    if identity.volume_serial_number == 0
        || identity.file_id.iter().all(|byte| *byte == 0)
        || identity.file_id.iter().all(|byte| *byte == 0xff)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the filesystem did not provide a stable volume and unique 128-bit file identity",
        ));
    }
    Ok(identity)
}

#[cfg(windows)]
fn compare_windows_file_identity_results(
    left: std::io::Result<WindowsFileIdentity>,
    right: std::io::Result<WindowsFileIdentity>,
) -> std::io::Result<bool> {
    Ok(validate_windows_file_identity(left?)? == validate_windows_file_identity(right?)?)
}

#[cfg(windows)]
fn query_windows_file_identity(file: &File) -> std::io::Result<WindowsFileIdentity> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx},
    };

    let mut information = MaybeUninit::<FILE_ID_INFO>::zeroed();
    // SAFETY: the file handle remains live and the output buffer has the exact
    // FILE_ID_INFO layout and size required by GetFileInformationByHandleEx.
    let success = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileIdInfo,
            information.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if success == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a successful call initialized the complete fixed-size output.
    let information = unsafe { information.assume_init() };
    validate_windows_file_identity(WindowsFileIdentity {
        volume_serial_number: information.VolumeSerialNumber,
        file_id: information.FileId.Identifier,
    })
}

#[cfg(windows)]
fn same_opened_file_identity(left: &File, right: &File) -> std::io::Result<bool> {
    // FILE_ID_INFO combines the volume serial with a 128-bit file identifier.
    // FileInternalInformation's 64-bit IndexNumber is documented as unique
    // only on NTFS and therefore cannot establish identity on every supported
    // Windows filesystem.
    compare_windows_file_identity_results(
        query_windows_file_identity(left),
        query_windows_file_identity(right),
    )
}

#[cfg(unix)]
fn same_opened_file_identity(left: &File, right: &File) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt as _;

    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.dev() == right.dev() && left.ino() == right.ino())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn checked_workspace_process_cwd(_: &CheckedWorkspacePath) -> Result<CheckedWorkspaceCwd, String> {
    unsupported_unix_operation()
}

#[cfg(not(any(unix, windows)))]
fn checked_workspace_process_cwd(_: &CheckedWorkspacePath) -> Result<CheckedWorkspaceCwd, String> {
    Err("secure workspace process directories are unavailable on this platform".into())
}

struct WorkspaceDirectoryEnumeration {
    directory: File,
    path: PathBuf,
    _cwd_guard: Option<CheckedWorkspaceCwd>,
}

fn read_workspace_directory_from_entry(
    path: &CheckedWorkspacePath,
    entry: &CheckedWorkspaceEntry,
) -> Result<Vec<CheckedWorkspaceDirEntry>, String> {
    if !entry.metadata.is_dir() {
        return Err(format!("'{}' is not a directory", path.requested.display()));
    }
    reverify_checked_workspace_entry(entry, path)?;
    let enumeration = open_workspace_directory_for_enumeration(path, entry)
        .map_err(|error| checked_relative_open_error(&path.requested, error))?;
    let entries = enumerate_workspace_directory(&enumeration.path, &path.requested)?;
    let namespace = Arc::new(CheckedWorkspaceNamespace {
        file: enumeration
            .directory
            .try_clone()
            .map_err(|error| checked_relative_open_error(&path.requested, error))?,
        relative: path.relative.clone(),
    });
    let mut checked = Vec::with_capacity(entries.len());
    for (name, is_link_or_reparse) in entries {
        if is_bootstrap_component(&name) {
            continue;
        }
        let mut relative = path.relative.clone();
        relative.push(&name);
        let mut child = CheckedWorkspacePath {
            root: Arc::clone(&path.root),
            requested: path.root.requested.join(&relative),
            relative,
            namespace: Arc::clone(&namespace),
            suffix: PathBuf::from(&name),
            probe: None,
        };
        if is_link_or_reparse {
            checked.push(CheckedWorkspaceDirEntry {
                name,
                path: child,
                kind: CheckedWorkspaceDirEntryKind::LinkOrReparse,
                metadata: None,
            });
            continue;
        }
        let Some(opened) = child.open_entry()? else {
            checked.push(CheckedWorkspaceDirEntry {
                name,
                path: child,
                kind: CheckedWorkspaceDirEntryKind::Missing,
                metadata: None,
            });
            continue;
        };
        child.probe =
            Some(Arc::new(opened.file.try_clone().map_err(|error| {
                checked_relative_open_error(&child.requested, error)
            })?));
        let kind = if opened.metadata.is_file() {
            CheckedWorkspaceDirEntryKind::File
        } else if opened.metadata.is_dir() {
            CheckedWorkspaceDirEntryKind::Directory
        } else {
            CheckedWorkspaceDirEntryKind::Other
        };
        checked.push(CheckedWorkspaceDirEntry {
            name,
            path: child,
            kind,
            metadata: Some(opened.metadata),
        });
    }
    reverify_checked_workspace_entry(entry, path)?;
    drop(enumeration);
    Ok(checked)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn open_workspace_directory_for_enumeration(
    _: &CheckedWorkspacePath,
    entry: &CheckedWorkspaceEntry,
) -> std::io::Result<WorkspaceDirectoryEnumeration> {
    use std::os::fd::AsRawFd as _;

    let directory = entry.file.try_clone()?;
    Ok(WorkspaceDirectoryEnumeration {
        path: PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())),
        directory,
        _cwd_guard: None,
    })
}

#[cfg(windows)]
fn open_workspace_directory_for_enumeration(
    path: &CheckedWorkspacePath,
    _: &CheckedWorkspaceEntry,
) -> std::io::Result<WorkspaceDirectoryEnumeration> {
    let cwd = checked_workspace_process_cwd(path).map_err(std::io::Error::other)?;
    let directory = cwd
        .anchors
        .last()
        .ok_or_else(|| std::io::Error::other("workspace directory lock chain was lost"))?
        .file
        .try_clone()?;
    Ok(WorkspaceDirectoryEnumeration {
        path: cwd.path.clone(),
        directory,
        _cwd_guard: Some(cwd),
    })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn open_workspace_directory_for_enumeration(
    _: &CheckedWorkspacePath,
    _: &CheckedWorkspaceEntry,
) -> std::io::Result<WorkspaceDirectoryEnumeration> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure mount-bound directory enumeration is unavailable on this Unix platform",
    ))
}

#[cfg(not(any(unix, windows)))]
fn open_workspace_directory_for_enumeration(
    _: &CheckedWorkspacePath,
    _: &CheckedWorkspaceEntry,
) -> std::io::Result<WorkspaceDirectoryEnumeration> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure workspace directory enumeration is unavailable on this platform",
    ))
}

#[cfg(any(target_os = "linux", target_os = "android", windows))]
fn enumerate_workspace_directory(
    path: &Path,
    requested: &Path,
) -> Result<Vec<(std::ffi::OsString, bool)>, String> {
    let mut entries = std::fs::read_dir(path)
        .map_err(|error| format!("cannot list '{}': {error}", requested.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot list '{}': {error}", requested.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    entries
        .into_iter()
        .map(|entry| {
            let file_type = entry.file_type().map_err(|error| {
                format!(
                    "cannot inspect '{}/{}': {error}",
                    requested.display(),
                    entry.file_name().to_string_lossy()
                )
            })?;
            Ok((entry.file_name(), file_type.is_symlink()))
        })
        .collect()
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn enumerate_workspace_directory(
    _: &Path,
    _: &Path,
) -> Result<Vec<(std::ffi::OsString, bool)>, String> {
    unsupported_unix_operation()
}

#[cfg(not(any(unix, windows)))]
fn enumerate_workspace_directory(
    _: &Path,
    _: &Path,
) -> Result<Vec<(std::ffi::OsString, bool)>, String> {
    Err("secure workspace directory enumeration is unavailable on this platform".into())
}

fn verify_checked_workspace_entry(entry: &CheckedWorkspaceEntry) -> Result<(), String> {
    verify_checked_workspace_root(&entry.root)?;
    let metadata = entry.file.metadata().map_err(|error| {
        format!(
            "cannot recheck workspace path '{}': {error}",
            entry.requested.display()
        )
    })?;
    if metadata_is_link_or_reparse(&metadata) {
        return Err(format!(
            "workspace path '{}' became a link or reparse point",
            entry.requested.display()
        ));
    }
    let actual = opened_file_path(&entry.file, &entry.requested).map_err(|error| {
        format!(
            "cannot verify workspace path '{}': {error}",
            entry.requested.display()
        )
    })?;
    if !paths_are_same(&actual, &entry.requested) {
        return Err(format!(
            "path '{}' changed while its checked handle was open",
            entry.requested.display()
        ));
    }
    Ok(())
}

pub(crate) fn reverify_checked_workspace_entry(
    entry: &CheckedWorkspaceEntry,
    path: &CheckedWorkspacePath,
) -> Result<(), String> {
    if !Arc::ptr_eq(&path.root, &entry.root)
        || path.relative != entry.relative
        || !paths_are_same(&path.requested, &entry.requested)
    {
        return Err(
            "checked workspace entry identity does not match its retained root anchor".into(),
        );
    }
    verify_checked_workspace_entry(entry)
}

fn open_checked_workspace_entry_with_hook(
    path: &CheckedWorkspacePath,
    hook: &mut dyn FnMut(),
) -> Result<Option<CheckedWorkspaceEntry>, String> {
    hook();
    verify_checked_workspace_root(&path.root)?;
    let open_once = || -> Result<Option<File>, String> {
        #[cfg(windows)]
        let result = if path.suffix.as_os_str().is_empty() {
            path.namespace.file.try_clone()
        } else {
            open_relative_from_namespace(path)
        };
        #[cfg(not(windows))]
        let result = if let Some(probe) = path.probe.as_deref() {
            probe.try_clone()
        } else if path.suffix.as_os_str().is_empty() {
            path.namespace.file.try_clone()
        } else {
            open_relative_from_namespace(path)
        };
        match result {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(checked_relative_open_error(&path.requested, error)),
        }
    };
    let Some(file) = open_once()? else {
        verify_checked_workspace_root(&path.root)?;
        if open_once()?.is_some() {
            return Err(format!(
                "workspace path '{}' changed while it was being checked",
                path.requested.display()
            ));
        }
        verify_checked_workspace_root(&path.root)?;
        return Ok(None);
    };
    #[cfg(windows)]
    if let Some(probe) = path.probe.as_deref()
        && !same_opened_file_identity(&file, probe)
            .map_err(|error| checked_relative_open_error(&path.requested, error))?
    {
        return Err(format!(
            "workspace path '{}' changed while its checked entry was opened",
            path.requested.display()
        ));
    }
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect '{}': {error}", path.requested.display()))?;
    if metadata_is_link_or_reparse(&metadata) {
        return Err(format!(
            "path '{}' is a link or reparse point",
            path.requested.display()
        ));
    }
    let entry = CheckedWorkspaceEntry {
        file,
        metadata,
        _operation_anchors: Vec::new(),
        root: Arc::clone(&path.root),
        relative: path.relative.clone(),
        requested: path.requested.clone(),
    };
    verify_checked_workspace_entry(&entry)?;
    Ok(Some(entry))
}

/// Inspect a capability relative to its unfollowed persisted workspace root.
/// Linux and Android use openat2 with NO_XDEV; Windows uses NtCreateFile
/// relative to the retained root handle. Other Unix targets fail closed rather
/// than treating st_dev as proof of mount identity. No later canonicalize call
/// can replace the retained trust boundary.
pub(crate) fn open_checked_workspace_entry(
    path: &CheckedWorkspacePath,
) -> Result<Option<CheckedWorkspaceEntry>, String> {
    path.open_entry()
}

#[cfg(test)]
pub(crate) fn open_checked_workspace_entry_after_root_hook(
    path: &CheckedWorkspacePath,
    root_hook: &mut dyn FnMut(),
) -> Result<Option<CheckedWorkspaceEntry>, String> {
    open_checked_workspace_entry_with_hook(path, root_hook)
}

/// Open an already-resolved workspace file without following the final link,
/// then verify the opened handle still points beneath the bound workspace.
/// Keeping the handle open closes the metadata/read TOCTOU window for callers.
pub(crate) fn open_checked_workspace_file(
    path: &CheckedWorkspacePath,
) -> Result<(CheckedWorkspaceFile, u64), String> {
    path.open_file_for_read()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "lingclaw-safety-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create safety test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(windows)]
    fn create_directory_link(link: &Path, target: &Path) {
        let output = std::process::Command::new("cmd.exe")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .output()
            .expect("run mklink");
        assert!(
            output.status.success(),
            "create workspace junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    fn create_directory_link(link: &Path, target: &Path) {
        std::os::unix::fs::symlink(target, link).expect("create directory symlink");
    }

    #[cfg(windows)]
    fn remove_directory_link(link: &Path) {
        fs::remove_dir(link).expect("remove directory junction");
    }

    #[cfg(unix)]
    fn remove_directory_link(link: &Path) {
        fs::remove_file(link).expect("remove directory symlink");
    }

    #[cfg(unix)]
    #[test]
    fn checked_resolution_does_not_require_read_access_for_a_write_only_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = TestDirectory::new("write-only");
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).expect("create workspace");
        let target = workspace.join("write-only.txt");
        fs::write(&target, b"before").expect("seed write-only file");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o200))
            .expect("make file write-only");

        let checked = resolve_path_checked("write-only.txt", &workspace)
            .expect("write-only target should resolve");
        assert!(
            checked.open_file_for_read().is_err(),
            "read_file must still require data-read permission"
        );
        let mut file = checked
            .open_file_for_write()
            .expect("write operation should request only write permission");
        std::io::Write::write_all(&mut file, b"after").expect("write through checked handle");
        drop(file);

        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))
            .expect("restore test-file permissions");
        assert_eq!(fs::read(&target).expect("read restored file"), b"after");

        let deletable = workspace.join("delete-without-read.txt");
        fs::write(&deletable, b"delete").expect("seed deletable file");
        fs::set_permissions(&deletable, fs::Permissions::from_mode(0o000))
            .expect("remove file permissions");
        let checked = resolve_path_checked("delete-without-read.txt", &workspace)
            .expect("delete target should resolve without read permission");
        checked
            .remove_file()
            .expect("delete should use the checked parent capability");
        assert!(!deletable.exists());
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn openat2_workspace_policy_includes_every_required_resolution_flag() {
        let how = workspace_open_how(libc::O_PATH | libc::O_CLOEXEC, 0);
        assert_eq!(std::mem::size_of::<WorkspaceOpenHow>(), 24);
        assert_eq!(
            how.resolve,
            WORKSPACE_RESOLVE_BENEATH
                | WORKSPACE_RESOLVE_NO_SYMLINKS
                | WORKSPACE_RESOLVE_NO_MAGICLINKS
                | WORKSPACE_RESOLVE_NO_XDEV
        );
        assert_ne!(how.resolve & WORKSPACE_RESOLVE_NO_XDEV, 0);
        assert_ne!(how.flags & libc::O_PATH as u64, 0);
        #[cfg(target_os = "linux")]
        assert_eq!(
            how.resolve,
            libc::RESOLVE_BENEATH
                | libc::RESOLVE_NO_SYMLINKS
                | libc::RESOLVE_NO_MAGICLINKS
                | libc::RESOLVE_NO_XDEV
        );
    }

    #[cfg(unix)]
    #[test]
    fn checked_process_cwd_accepts_a_search_only_directory() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = TestDirectory::new("search-only-cwd");
        let workspace = temp.path().join("workspace");
        let target = workspace.join("search-only");
        fs::create_dir_all(&target).expect("create search-only directory");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o111))
            .expect("make directory search-only");
        let checked = resolve_path_checked("search-only", &workspace)
            .expect("search-only directory should resolve");
        let cwd = checked
            .process_cwd()
            .expect("cwd capability should require traverse, not listing");
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 0")
            .current_dir(cwd.path())
            .status()
            .expect("spawn in search-only directory");
        drop(cwd);
        fs::set_permissions(&target, fs::Permissions::from_mode(0o700))
            .expect("restore directory permissions");
        assert!(status.success());
    }

    #[cfg(windows)]
    #[test]
    fn checked_resolution_allows_an_existing_delete_shared_handle() {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        let temp = TestDirectory::new("delete-share");
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).expect("create workspace");
        let target = workspace.join("shared.txt");
        fs::write(&target, b"shared").expect("seed shared file");
        let held = fs::OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&target)
            .expect("open test file with DELETE access");

        let result = resolve_path_checked("shared.txt", &workspace);

        assert!(
            result.is_ok(),
            "namespace validation must preserve delete sharing: {result:?}"
        );
        let checked = result.expect("delete-shared file should resolve");
        let (mut file, _) = checked
            .open_file_for_read()
            .expect("actual read should also preserve delete sharing");
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut file, &mut contents).expect("read delete-shared file");
        assert_eq!(contents, "shared");
        drop(file);
        checked
            .remove_file()
            .expect("actual delete should preserve delete sharing");
        drop(held);
        assert!(!target.exists());
    }

    #[cfg(windows)]
    #[test]
    fn checked_write_uses_no_file_data_read_access_on_windows() {
        let temp = TestDirectory::new("write-acl");
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).expect("create workspace");
        let target = workspace.join("write-only.txt");
        fs::write(&target, b"before").expect("seed ACL test file");
        let identity = std::process::Command::new("whoami.exe")
            .output()
            .expect("query current Windows identity");
        assert!(identity.status.success());
        let identity = String::from_utf8_lossy(&identity.stdout).trim().to_string();
        let restrict = std::process::Command::new("icacls.exe")
            .arg(&target)
            .arg("/inheritance:r")
            .arg("/grant:r")
            .arg(format!("{identity}:(W)"))
            .output()
            .expect("restrict ACL to write access");
        assert!(
            restrict.status.success(),
            "icacls restriction failed: {}",
            String::from_utf8_lossy(&restrict.stderr)
        );

        let result = resolve_path_checked("write-only.txt", &workspace).and_then(|checked| {
            let mut file = checked.open_file_for_write()?;
            std::io::Write::write_all(&mut file, b"after").map_err(|error| error.to_string())
        });

        let restore = std::process::Command::new("icacls.exe")
            .arg(&target)
            .arg("/grant:r")
            .arg(format!("{identity}:(F)"))
            .output()
            .expect("restore ACL");
        assert!(restore.status.success(), "restore ACL before asserting");
        result.expect("checked write must not request FILE_READ_DATA");
        assert_eq!(fs::read(&target).expect("read restored file"), b"after");
    }

    #[cfg(windows)]
    #[test]
    fn existing_file_operations_reject_a_replacement_after_resolution() {
        fn replace_after_resolution(
            workspace: &Path,
            name: &str,
        ) -> (CheckedWorkspacePath, PathBuf, PathBuf) {
            let target = workspace.join(name);
            let moved = workspace.join(format!("moved-{name}"));
            let replacement = workspace.join(format!("replacement-{name}"));
            fs::write(&target, b"original").expect("seed original target");
            fs::write(&replacement, b"replacement").expect("seed replacement target");
            let checked = resolve_path_checked(name, workspace).expect("resolve original target");
            fs::rename(&target, &moved).expect("move resolved target");
            fs::rename(&replacement, &target).expect("install replacement target");
            (checked, target, moved)
        }

        let temp = TestDirectory::new("file-swap");
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).expect("create swap workspace");

        let (read, read_replacement, read_original) =
            replace_after_resolution(&workspace, "read.txt");
        assert!(read.open_file_for_read().is_err());
        assert_eq!(
            fs::read(read_replacement).expect("read replacement"),
            b"replacement"
        );
        assert_eq!(fs::read(read_original).expect("read original"), b"original");

        let (write, write_replacement, write_original) =
            replace_after_resolution(&workspace, "write.txt");
        assert!(write.open_file_for_write().is_err());
        assert_eq!(
            fs::read(write_replacement).expect("read write replacement"),
            b"replacement"
        );
        assert_eq!(
            fs::read(write_original).expect("read moved write original"),
            b"original"
        );

        let (patch, patch_replacement, patch_original) =
            replace_after_resolution(&workspace, "patch.txt");
        assert!(patch.open_file_for_patch().is_err());
        assert_eq!(
            fs::read(patch_replacement).expect("read patch replacement"),
            b"replacement"
        );
        assert_eq!(
            fs::read(patch_original).expect("read moved patch original"),
            b"original"
        );

        let (delete, delete_replacement, delete_original) =
            replace_after_resolution(&workspace, "delete.txt");
        assert!(delete.remove_file().is_err());
        assert_eq!(
            fs::read(delete_replacement).expect("read delete replacement"),
            b"replacement"
        );
        assert_eq!(
            fs::read(delete_original).expect("read moved delete original"),
            b"original"
        );
    }

    #[cfg(windows)]
    #[test]
    fn final_probe_is_parent_relative_when_an_intermediate_is_replaced() {
        for (operation, target_exists) in [
            ("read", true),
            ("write", true),
            ("patch", true),
            ("delete", true),
            ("create", false),
        ] {
            let temp = TestDirectory::new(&format!("parent-relative-probe-{operation}"));
            let workspace = temp.path().join("workspace");
            let subtree = workspace.join("subtree");
            let moved_subtree = workspace.join("subtree-original");
            let outside = temp.path().join("outside");
            fs::create_dir_all(&subtree).expect("create checked subtree");
            fs::create_dir_all(&outside).expect("create outside subtree");
            let file_name = format!("{operation}.txt");
            if target_exists {
                fs::write(subtree.join(&file_name), b"inside-original")
                    .expect("seed checked target");
            }
            fs::write(outside.join(&file_name), b"outside-sentinel")
                .expect("seed outside sentinel");

            let mut replacement_installed = false;
            let result = resolve_path_checked_with_probe_hook(
                &format!("subtree/{file_name}"),
                &workspace,
                &mut || {
                    fs::rename(&subtree, &moved_subtree)
                        .expect("move checked parent at final-probe barrier");
                    create_directory_link(&subtree, &outside);
                    replacement_installed = true;
                },
            )
            .and_then(|checked| match operation {
                "read" => {
                    let (mut file, _) = checked.open_file_for_read()?;
                    let mut byte = [0_u8; 1];
                    std::io::Read::read_exact(&mut file, &mut byte)
                        .map_err(|error| error.to_string())
                }
                "write" | "create" => {
                    let mut file = checked.open_file_for_write()?;
                    std::io::Write::write_all(&mut file, b"unexpected")
                        .map_err(|error| error.to_string())
                }
                "patch" => checked
                    .open_file_for_patch()?
                    .set_len(0)
                    .map_err(|error| error.to_string()),
                "delete" => checked.remove_file(),
                _ => unreachable!("test operation is fixed"),
            });

            assert!(replacement_installed, "test barrier must install junction");
            assert!(
                result.is_err(),
                "{operation} must fail closed after its checked parent moves"
            );
            assert_eq!(
                fs::read(outside.join(&file_name)).expect("read outside sentinel"),
                b"outside-sentinel",
                "{operation} must never acquire or modify the junction target"
            );
            if target_exists {
                assert_eq!(
                    fs::read(moved_subtree.join(&file_name)).expect("read moved inside target"),
                    b"inside-original"
                );
            } else {
                assert!(
                    !moved_subtree.join(&file_name).exists(),
                    "create probe must not create in the moved original parent"
                );
            }
            remove_directory_link(&subtree);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android", windows))]
    #[test]
    fn operation_barriers_prevent_side_effects_after_a_namespace_move() {
        let temp = TestDirectory::new("operation-barrier");
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).expect("create barrier workspace");

        let target = workspace.join("write.txt");
        let moved = workspace.join("write-moved.txt");
        let replacement_source = workspace.join("write-replacement-source.txt");
        fs::write(&target, b"original").expect("seed write target");
        fs::write(&replacement_source, b"replacement").expect("seed write replacement");
        let checked = resolve_path_checked("write.txt", &workspace).expect("resolve write target");
        let mut moved_during_hook = false;
        let result = checked.open_file_for_write_after_lock_hook(&mut || {
            if fs::rename(&target, &moved).is_ok() {
                moved_during_hook = true;
                fs::rename(&replacement_source, &target).expect("install write replacement");
            }
        });
        if moved_during_hook {
            assert!(result.is_err(), "moved write target must fail closed");
            assert_eq!(fs::read(&moved).expect("read moved original"), b"original");
            assert_eq!(fs::read(&target).expect("read replacement"), b"replacement");
        } else {
            #[cfg(windows)]
            {
                drop(result.expect("locked Windows write should proceed"));
                assert_eq!(fs::read(&target).expect("read locked target"), b"");
                assert_eq!(
                    fs::read(&replacement_source).expect("read unused replacement"),
                    b"replacement"
                );
            }
        }

        let patch_target = workspace.join("patch.txt");
        let patch_moved = workspace.join("patch-moved.txt");
        fs::write(&patch_target, b"patch-original").expect("seed patch target");
        let patch = resolve_path_checked("patch.txt", &workspace).expect("resolve patch target");
        let patch_file = patch
            .open_file_for_patch()
            .expect("acquire patch capability");
        let patch_moved_during_operation = fs::rename(&patch_target, &patch_moved).is_ok();
        let patch_result = patch_file.set_len(0);
        if patch_moved_during_operation {
            assert!(patch_result.is_err(), "moved patch target must fail closed");
            assert_eq!(
                fs::read(&patch_moved).expect("read moved patch target"),
                b"patch-original"
            );
        } else {
            #[cfg(windows)]
            assert!(patch_result.is_ok(), "locked Windows patch should proceed");
        }
        drop(patch_file);

        let delete_target = workspace.join("delete.txt");
        let delete_moved = workspace.join("delete-moved.txt");
        fs::write(&delete_target, b"delete-original").expect("seed delete target");
        let delete = resolve_path_checked("delete.txt", &workspace).expect("resolve delete target");
        let mut delete_moved_during_hook = false;
        let delete_result = delete.remove_file_after_lock_hook(&mut || {
            if fs::rename(&delete_target, &delete_moved).is_ok() {
                delete_moved_during_hook = true;
            }
        });
        if delete_moved_during_hook {
            assert!(
                delete_result.is_err(),
                "moved delete target must fail closed"
            );
            assert_eq!(
                fs::read(&delete_moved).expect("read moved delete target"),
                b"delete-original"
            );
        } else {
            #[cfg(windows)]
            assert!(
                delete_result.is_ok(),
                "locked Windows delete should proceed"
            );
        }

        let parent = workspace.join("create-parent");
        let moved_parent = workspace.join("create-parent-moved");
        fs::create_dir(&parent).expect("create target parent");
        let create = resolve_path_checked("create-parent/new.txt", &workspace)
            .expect("resolve missing create target");
        let mut parent_moved_during_hook = false;
        let create_result = create.open_file_for_write_after_lock_hook(&mut || {
            if fs::rename(&parent, &moved_parent).is_ok() {
                parent_moved_during_hook = true;
                fs::create_dir(&parent).expect("install replacement parent");
            }
        });
        if parent_moved_during_hook {
            assert!(
                create_result.is_err(),
                "moved create parent must fail closed"
            );
            assert!(!parent.join("new.txt").exists());
            assert!(!moved_parent.join("new.txt").exists());
        } else {
            #[cfg(windows)]
            {
                drop(create_result.expect("locked Windows create should proceed"));
                assert!(parent.join("new.txt").exists());
            }
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn resolved_workspace_path_cannot_follow_a_replaced_root_before_io() {
        let temp = TestDirectory::new("root-race");
        let workspace = temp.path().join("workspace");
        let original_workspace = temp.path().join("workspace-original");
        let outside = temp.path().join("outside");
        fs::create_dir(&workspace).expect("create workspace");
        fs::create_dir(&outside).expect("create outside directory");
        fs::write(workspace.join("probe.txt"), b"inside").expect("write inside probe");
        fs::write(outside.join("probe.txt"), b"outside").expect("write outside probe");

        let resolved = resolve_path_checked("probe.txt", &workspace)
            .expect("resolve probe beneath original workspace");
        let renamed = fs::rename(&workspace, &original_workspace).is_ok();

        #[cfg(unix)]
        if renamed {
            std::os::unix::fs::symlink(&outside, &workspace)
                .expect("replace workspace path with symlink");
        }
        #[cfg(windows)]
        if renamed {
            let output = std::process::Command::new("cmd.exe")
                .arg("/c")
                .arg("mklink")
                .arg("/J")
                .arg(&workspace)
                .arg(&outside)
                .output()
                .expect("run mklink");
            assert!(
                output.status.success(),
                "create workspace junction: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let observed = resolved.open_file_for_read().and_then(|(mut file, _)| {
            let mut contents = String::new();
            std::io::Read::read_to_string(&mut file, &mut contents)
                .map_err(|error| error.to_string())?;
            Ok(contents)
        });

        #[cfg(unix)]
        if renamed {
            fs::remove_file(&workspace).expect("remove workspace symlink");
        }
        #[cfg(windows)]
        if renamed {
            fs::remove_dir(&workspace).expect("remove workspace junction");
        }
        if renamed {
            assert!(
                observed.is_err(),
                "I/O must fail closed after its workspace root moves: {observed:?}"
            );
        } else {
            #[cfg(windows)]
            assert_eq!(
                observed.as_deref(),
                Ok("inside"),
                "the retained Windows root handle must prevent the rename"
            );
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn checked_operations_do_not_follow_a_replaced_intermediate_directory() {
        let temp = TestDirectory::new("intermediate-race");
        let workspace = temp.path().join("workspace");
        let original_subtree = temp.path().join("subtree-original");
        let outside = temp.path().join("outside");
        fs::create_dir_all(workspace.join("subtree")).expect("create workspace subtree");
        fs::create_dir_all(&outside).expect("create outside subtree");
        fs::write(workspace.join("subtree/read.txt"), b"inside").expect("seed inside file");
        fs::write(outside.join("read.txt"), b"outside").expect("seed outside read file");
        fs::write(outside.join("delete.txt"), b"outside-delete").expect("seed outside delete file");

        let read = resolve_path_checked("subtree/read.txt", &workspace).expect("resolve read");
        let write = resolve_path_checked("subtree/create.txt", &workspace).expect("resolve create");
        let delete =
            resolve_path_checked("subtree/delete.txt", &workspace).expect("resolve delete");
        let directory = resolve_path_checked("subtree", &workspace).expect("resolve directory");

        if let Err(error) = fs::rename(workspace.join("subtree"), &original_subtree) {
            #[cfg(windows)]
            {
                assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
                drop(read);
                drop(write);
                drop(delete);
                drop(directory);
                return;
            }
            #[cfg(not(windows))]
            panic!("rename checked subtree: {error}");
        }
        create_directory_link(&workspace.join("subtree"), &outside);

        assert!(read.open_file_for_read().is_err());
        assert!(write.open_file_for_write().is_err());
        assert!(delete.remove_file().is_err());
        assert!(directory.read_directory().is_err());
        assert!(directory.process_cwd().is_err());
        assert!(!outside.join("create.txt").exists());
        assert!(!original_subtree.join("create.txt").exists());
        assert_eq!(
            fs::read(outside.join("delete.txt")).expect("outside delete evidence remains"),
            b"outside-delete"
        );

        remove_directory_link(&workspace.join("subtree"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn exported_child_root_is_not_inherited_by_an_unrelated_process() {
        let temp = TestDirectory::new("child-root-cloexec");
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).expect("create child-root workspace");
        fs::write(workspace.join("identity.txt"), b"inside").expect("seed child-root identity");
        let checked = resolve_path_checked(".", &workspace).expect("resolve child-root workspace");
        let child_root = checked
            .export_to_child()
            .expect("export checked child root");

        let status = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg("test ! -e \"$1/identity.txt\"")
            .arg("lingclaw-unrelated-child")
            .arg(child_root.path())
            .status()
            .expect("spawn unrelated child process");

        assert!(
            status.success(),
            "an unrelated child must not inherit the exported workspace descriptor"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_file_identity_uses_volume_and_all_128_identifier_bits() {
        fn identity(volume: u64, low: u64, high: u64) -> WindowsFileIdentity {
            let mut file_id = [0_u8; 16];
            file_id[..8].copy_from_slice(&low.to_le_bytes());
            file_id[8..].copy_from_slice(&high.to_le_bytes());
            WindowsFileIdentity {
                volume_serial_number: volume,
                file_id,
            }
        }

        let baseline = identity(7, 11, 13);
        assert!(
            compare_windows_file_identity_results(Ok(baseline), Ok(baseline))
                .expect("a stable identity should compare")
        );
        assert!(
            !compare_windows_file_identity_results(Ok(baseline), Ok(identity(8, 11, 13)))
                .expect("different volumes should compare")
        );
        assert!(
            !compare_windows_file_identity_results(Ok(baseline), Ok(identity(7, 11, 17)))
                .expect("the complete 128-bit identifier should compare"),
            "equal 64-bit low halves must not hide a different high half"
        );
        assert!(
            compare_windows_file_identity_results(
                Ok(WindowsFileIdentity {
                    volume_serial_number: 7,
                    file_id: [0; 16],
                }),
                Ok(baseline),
            )
            .is_err(),
            "an all-zero file identifier must fail closed"
        );
        assert!(
            compare_windows_file_identity_results(
                Ok(WindowsFileIdentity {
                    volume_serial_number: 7,
                    file_id: [0xff; 16],
                }),
                Ok(baseline),
            )
            .is_err(),
            "the all-FF non-unique FILE_ID_128 sentinel must fail closed"
        );
        let mut near_sentinel = [0xff; 16];
        near_sentinel[15] = 0xfe;
        let near_sentinel = WindowsFileIdentity {
            volume_serial_number: 7,
            file_id: near_sentinel,
        };
        assert!(
            compare_windows_file_identity_results(Ok(near_sentinel), Ok(near_sentinel))
                .expect("a non-sentinel 128-bit identifier should remain valid")
        );
        assert!(
            compare_windows_file_identity_results(Ok(identity(0, 11, 13)), Ok(baseline)).is_err(),
            "a zero volume identity must fail closed"
        );
        assert!(
            compare_windows_file_identity_results(
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "FILE_ID_INFO unavailable",
                )),
                Ok(baseline),
            )
            .is_err(),
            "an unsupported identity query must fail closed"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn checked_operations_support_normal_workspace_files_and_directories() {
        let temp = TestDirectory::new("normal-operations");
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).expect("create workspace");
        let target = resolve_path_checked("nested/result.txt", &workspace)
            .expect("resolve missing write target");
        let mut writer = target
            .open_file_for_write()
            .expect("create checked nested target");
        std::io::Write::write_all(&mut writer, b"before").expect("write checked target");
        drop(writer);

        let target =
            resolve_path_checked("nested/result.txt", &workspace).expect("resolve existing target");
        let (mut reader, _) = target.open_file_for_read().expect("read checked target");
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut reader, &mut contents).expect("read target bytes");
        assert_eq!(contents, "before");
        drop(reader);

        let mut patch = target.open_file_for_patch().expect("open patch handle");
        std::io::Seek::seek(&mut patch, std::io::SeekFrom::Start(0)).expect("seek patch handle");
        patch.set_len(0).expect("truncate patch target");
        std::io::Write::write_all(&mut patch, b"after").expect("patch target bytes");
        drop(patch);

        let directory = resolve_path_checked("nested", &workspace).expect("resolve directory");
        let cwd = directory
            .process_cwd()
            .expect("normal directory should be usable as a process cwd");
        #[cfg(windows)]
        assert_eq!(cwd.path(), directory.display_path());
        drop(cwd);
        let entries = directory.read_directory().expect("list checked directory");
        assert!(entries.iter().any(|entry| {
            entry.name == std::ffi::OsStr::new("result.txt")
                && entry.kind == CheckedWorkspaceDirEntryKind::File
        }));
        drop(entries);
        target.remove_file().expect("delete checked target");
        assert!(!workspace.join("nested/result.txt").exists());
    }
}
