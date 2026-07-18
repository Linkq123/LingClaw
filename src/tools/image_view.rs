use std::{
    fs::File,
    path::{Path, PathBuf},
};

use serde_json::Value;
use tokio::io::AsyncReadExt;

use super::{ToolImageOutput, mcp::ToolImageBudget, safety::resolve_path_checked};

pub(super) enum ToolViewImageOutcome {
    Image(ToolImageOutput),
    Skipped(String),
}

struct ToolImageReservation<'a> {
    budget: &'a ToolImageBudget,
    committed: bool,
}

impl ToolImageReservation<'_> {
    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for ToolImageReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.budget.release();
        }
    }
}

fn open_image_file(path: &Path) -> std::io::Result<File> {
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
            "image path resolves through a reparse point",
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

fn open_checked_workspace_file(
    resolved: &Path,
    workspace_root: &Path,
) -> Result<(File, u64), String> {
    let file = open_image_file(resolved)
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

pub(crate) async fn tool_view_image(
    args: &Value,
    workspace: &Path,
) -> Result<ToolImageOutput, String> {
    match tool_view_image_with_budget(args, workspace, None).await? {
        ToolViewImageOutcome::Image(image) => Ok(image),
        ToolViewImageOutcome::Skipped(reason) => Err(reason),
    }
}

pub(super) async fn tool_view_image_with_budget(
    args: &Value,
    workspace: &Path,
    image_budget: Option<&ToolImageBudget>,
) -> Result<ToolViewImageOutcome, String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| "missing required parameter 'path'".to_string())?;
    let resolved = resolve_path_checked(path, workspace)?;
    let workspace_root = workspace
        .canonicalize()
        .map_err(|error| format!("cannot verify workspace '{}': {error}", workspace.display()))?;
    let open_path = resolved.clone();
    let open_root = workspace_root.clone();
    let (file, file_len) =
        tokio::task::spawn_blocking(move || open_checked_workspace_file(&open_path, &open_root))
            .await
            .map_err(|error| format!("cannot open '{}': {error}", resolved.display()))??;
    if file_len > crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES as u64 {
        return Err(format!(
            "image exceeds the {} byte limit",
            crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES
        ));
    }

    let mut reservation = if let Some(image_budget) = image_budget {
        image_budget.wait_for_turn().await;
        if !image_budget.try_reserve() {
            return Ok(ToolViewImageOutcome::Skipped(
                "tool image batch limit reached".to_string(),
            ));
        }
        Some(ToolImageReservation {
            budget: image_budget,
            committed: false,
        })
    } else {
        None
    };

    let mut data = Vec::with_capacity(file_len as usize);
    let file = tokio::fs::File::from_std(file);
    file.take(crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .await
        .map_err(|error| format!("cannot read '{}': {error}", resolved.display()))?;
    if data.len() > crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES {
        return Err(format!(
            "image exceeds the {} byte limit",
            crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES
        ));
    }
    let mime_type = crate::image_uploads::detect_image_upload_content_type(&data)
        .ok_or_else(|| "only valid PNG and JPEG images are supported".to_string())?;
    let name = resolved
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("image")
        .to_string();

    let image = ToolImageOutput {
        data,
        mime_type: mime_type.to_string(),
        name,
    };
    if let Some(reservation) = reservation.as_mut() {
        reservation.commit();
    }

    Ok(ToolViewImageOutcome::Image(image))
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    use super::*;

    const ONE_PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII=";

    fn workspace(name: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!("lingclaw-view-image-{name}-{unique}"))
    }

    #[tokio::test]
    async fn reads_magic_valid_png_inside_workspace() {
        let workspace = workspace("valid");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let path = workspace.join("pixel.png");
        let bytes = STANDARD.decode(ONE_PIXEL_PNG).unwrap();
        tokio::fs::write(&path, &bytes).await.unwrap();

        let image = tool_view_image(&serde_json::json!({"path":"pixel.png"}), &workspace)
            .await
            .expect("valid workspace image");

        assert_eq!(image.data, bytes);
        assert_eq!(image.mime_type, "image/png");
        assert_eq!(image.name, "pixel.png");
        let _ = tokio::fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn shared_batch_budget_skips_before_loading_image_bytes() {
        let workspace = workspace("batch-limit");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let path = workspace.join("pixel.png");
        tokio::fs::write(&path, STANDARD.decode(ONE_PIXEL_PNG).unwrap())
            .await
            .unwrap();
        let budget = ToolImageBudget::new(0).for_call(0);

        let result = tool_view_image_with_budget(
            &serde_json::json!({"path":"pixel.png"}),
            &workspace,
            Some(&budget),
        )
        .await
        .expect("a valid over-budget image should be skipped cleanly");

        assert!(matches!(
            result,
            ToolViewImageOutcome::Skipped(reason)
                if reason.contains("tool image batch limit reached")
        ));
        let _ = tokio::fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn rejects_extension_only_fake_image() {
        let workspace = workspace("invalid");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(workspace.join("fake.png"), b"not an image")
            .await
            .unwrap();

        let error = tool_view_image(&serde_json::json!({"path":"fake.png"}), &workspace)
            .await
            .unwrap_err();

        assert!(error.contains("valid PNG and JPEG"));
        let _ = tokio::fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn rejects_images_larger_than_ten_megabytes_before_reading() {
        let workspace = workspace("large");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let path = workspace.join("large.png");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES as u64 + 1)
            .unwrap();

        let error = tool_view_image(&serde_json::json!({"path":"large.png"}), &workspace)
            .await
            .unwrap_err();

        assert!(error.contains("byte limit"));
        let _ = tokio::fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn rejects_paths_outside_workspace() {
        let workspace = workspace("outside");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        let result = tool_view_image(
            &serde_json::json!({"path": workspace.join("..").join("outside.png")}),
            &workspace,
        )
        .await;
        assert!(result.unwrap_err().contains("outside"));
        let _ = tokio::fs::remove_dir_all(workspace).await;
    }

    #[cfg(any(unix, windows))]
    #[tokio::test]
    async fn rejects_symbolic_links_before_reading() {
        let workspace = workspace("symlink");
        let outside = workspace.with_extension("outside.png");
        tokio::fs::create_dir_all(&workspace).await.unwrap();
        tokio::fs::write(&outside, STANDARD.decode(ONE_PIXEL_PNG).unwrap())
            .await
            .unwrap();
        let link = workspace.join("linked.png");
        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(&outside, &link);
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_file(&outside, &link);
        if cfg!(windows) && link_result.is_err() {
            // Windows may require Developer Mode or elevated symlink rights.
            let _ = tokio::fs::remove_file(outside).await;
            let _ = tokio::fs::remove_dir_all(workspace).await;
            return;
        }
        link_result.expect("symbolic link should be created");

        let error = tool_view_image(&serde_json::json!({"path":"linked.png"}), &workspace)
            .await
            .unwrap_err();

        assert!(error.contains("symlink"));
        let _ = tokio::fs::remove_file(link).await;
        let _ = tokio::fs::remove_file(outside).await;
        let _ = tokio::fs::remove_dir_all(workspace).await;
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn checked_handle_rejects_an_intermediate_symlink_escape() {
        let workspace = workspace("handle-symlink");
        let outside = workspace.with_extension("outside-dir");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("pixel.png"),
            STANDARD.decode(ONE_PIXEL_PNG).unwrap(),
        )
        .unwrap();
        let link = workspace.join("linked-dir");
        #[cfg(unix)]
        let link_result = std::os::unix::fs::symlink(&outside, &link);
        #[cfg(windows)]
        let link_result = std::os::windows::fs::symlink_dir(&outside, &link);
        if cfg!(windows) && link_result.is_err() {
            let _ = std::fs::remove_dir_all(outside);
            let _ = std::fs::remove_dir_all(workspace);
            return;
        }
        link_result.expect("directory symlink should be created");

        let root = workspace.canonicalize().unwrap();
        let error = open_checked_workspace_file(&link.join("pixel.png"), &root).unwrap_err();

        assert!(error.contains("outside"));
        #[cfg(unix)]
        let _ = std::fs::remove_file(&link);
        #[cfg(windows)]
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(outside);
        let _ = std::fs::remove_dir_all(workspace);
    }
}
