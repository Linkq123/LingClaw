use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use futures::stream::{self, StreamExt};

use regex::Regex;

use crate::tools::safety::{
    CheckedWorkspaceDirEntryKind, CheckedWorkspacePath, resolve_path_checked,
};
use crate::{Config, truncate};

pub(crate) fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub(crate) fn matches_glob(name: &str, pattern: &str) -> bool {
    if let Some(ext) = pattern.strip_prefix("*.") {
        name.ends_with(&format!(".{ext}"))
    } else if let Some(prefix) = pattern.strip_suffix('*') {
        name.starts_with(prefix)
    } else {
        name == pattern
    }
}

fn resolve_tool_path(
    path_str: &str,
    workspace: &Path,
    tool_name: &str,
) -> Result<CheckedWorkspacePath, String> {
    resolve_path_checked(path_str, workspace)
        .map_err(|message| format!("{tool_name} error: {message}"))
}

enum ReadableToolPath {
    Virtual(PathBuf),
    Workspace(CheckedWorkspacePath),
}

fn read_checked_workspace_text(path: CheckedWorkspacePath) -> std::io::Result<String> {
    let (mut file, _) = path.open_file_for_read().map_err(std::io::Error::other)?;
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

fn write_checked_workspace_text(
    path: CheckedWorkspacePath,
    content: &str,
) -> std::io::Result<usize> {
    let mut file = path.open_file_for_write().map_err(std::io::Error::other)?;
    file.write_all(content.as_bytes())?;
    file.flush()?;
    Ok(content.len())
}

fn patch_checked_workspace_text(
    path: CheckedWorkspacePath,
    old_str: &str,
    new_str: &str,
) -> Result<usize, String> {
    let mut file = path.open_file_for_patch()?;
    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|error| format!("read error: {error}"))?;
    let count = content.matches(old_str).count();
    if count == 0 {
        return Err("old_string not found".to_string());
    }
    let new_content = content.replacen(old_str, new_str, 1);
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("seek error: {error}"))?;
    file.set_len(0)
        .map_err(|error| format!("truncate error: {error}"))?;
    file.write_all(new_content.as_bytes())
        .map_err(|error| format!("write error: {error}"))?;
    file.flush()
        .map_err(|error| format!("flush error: {error}"))?;
    Ok(count)
}

fn list_checked_workspace_directory(
    path: CheckedWorkspacePath,
) -> Result<Vec<crate::tools::safety::CheckedWorkspaceDirEntry>, String> {
    path.read_directory()
}

fn delete_checked_workspace_file(path: CheckedWorkspacePath) -> Result<(), String> {
    path.remove_file()
}

impl ReadableToolPath {
    fn display_path(&self) -> &Path {
        match self {
            Self::Virtual(path) => path,
            Self::Workspace(path) => path.display_path(),
        }
    }
}

/// Like `resolve_tool_path` but also resolves virtual skill paths
/// (`system://skills/...`, `~/.lingclaw/skills/...`) for read-only access.
fn resolve_tool_path_readable(
    path_str: &str,
    workspace: &Path,
    tool_name: &str,
) -> Result<ReadableToolPath, String> {
    // Try virtual skill path first (read-only)
    if let Some(real) = crate::prompts::resolve_skill_path(path_str) {
        return Ok(ReadableToolPath::Virtual(real));
    }
    resolve_tool_path(path_str, workspace, tool_name).map(ReadableToolPath::Workspace)
}

// ── read_file ────────────────────────────────────────────────────────────────

pub(crate) async fn tool_read_file(
    args: &serde_json::Value,
    config: &Config,
    workspace: &Path,
) -> String {
    let path_str = match args["path"].as_str() {
        Some(p) => p,
        None => return "Error: 'path' parameter is required".into(),
    };
    let path = match resolve_tool_path_readable(path_str, workspace, "read_file") {
        Ok(path) => path,
        Err(message) => return message,
    };
    let display_path = path.display_path().to_path_buf();

    let read_result = match path {
        ReadableToolPath::Virtual(path) => tokio::fs::read_to_string(path).await,
        ReadableToolPath::Workspace(path) => {
            tokio::task::spawn_blocking(move || read_checked_workspace_text(path))
                .await
                .map_err(std::io::Error::other)
                .and_then(|result| result)
        }
    };
    match read_result {
        Ok(content) => {
            let start = args["start_line"].as_u64().map(|n| n as usize);
            let end = args["end_line"].as_u64().map(|n| n as usize);
            if matches!(start, Some(0)) || matches!(end, Some(0)) {
                return "read_file error: start_line and end_line must be >= 1".into();
            }
            if let (Some(start), Some(end)) = (start, end)
                && end < start
            {
                return "read_file error: end_line must be greater than or equal to start_line"
                    .into();
            }
            let lines: Vec<&str> = content.lines().collect();
            let total = lines.len();

            match (start, end) {
                (Some(s), Some(e)) => {
                    let s = s.saturating_sub(1).min(total);
                    let e = e.min(total);
                    let numbered: Vec<String> = lines[s..e]
                        .iter()
                        .enumerate()
                        .map(|(i, l)| format!("{:>5} | {}", s + i + 1, l))
                        .collect();
                    let header = format!(
                        "[{} — lines {}-{} of {}]\n",
                        display_path.display(),
                        s + 1,
                        e,
                        total
                    );
                    truncate(
                        &format!("{header}{}", numbered.join("\n")),
                        config.max_file_bytes,
                    )
                }
                (Some(s), None) => {
                    let s = s.saturating_sub(1).min(total);
                    let numbered: Vec<String> = lines[s..]
                        .iter()
                        .enumerate()
                        .map(|(i, l)| format!("{:>5} | {}", s + i + 1, l))
                        .collect();
                    let header = format!(
                        "[{} — lines {}-{} of {}]\n",
                        display_path.display(),
                        s + 1,
                        total,
                        total
                    );
                    truncate(
                        &format!("{header}{}", numbered.join("\n")),
                        config.max_file_bytes,
                    )
                }
                _ => {
                    let header = format!("[{} — {} lines]\n", display_path.display(), total);
                    truncate(&format!("{header}{content}"), config.max_file_bytes)
                }
            }
        }
        Err(e) => format!("read_file error: {e}"),
    }
}

// ── write_file ───────────────────────────────────────────────────────────────

pub(crate) async fn tool_write_file(
    args: &serde_json::Value,
    _config: &Config,
    workspace: &Path,
) -> String {
    let path_str = match args["path"].as_str() {
        Some(p) => p,
        None => return "Error: 'path' parameter is required".into(),
    };
    let content = match args["content"].as_str() {
        Some(c) => c,
        None => return "Error: 'content' parameter is required".into(),
    };
    let path = match resolve_tool_path(path_str, workspace, "write_file") {
        Ok(path) => path,
        Err(message) => return message,
    };
    let display_path = path.display_path().to_path_buf();
    let content = content.to_string();

    let result =
        tokio::task::spawn_blocking(move || write_checked_workspace_text(path, &content)).await;
    match result {
        Ok(Ok(bytes)) => format!("Written {bytes} bytes to {}", display_path.display()),
        Err(error) => format!("write_file error: worker failed: {error}"),
        Ok(Err(e)) => format!("write_file error: {e}"),
    }
}

// ── patch_file ───────────────────────────────────────────────────────────────

pub(crate) async fn tool_patch_file(
    args: &serde_json::Value,
    _config: &Config,
    workspace: &Path,
) -> String {
    let path_str = match args["path"].as_str() {
        Some(p) => p,
        None => return "Error: 'path' parameter is required".into(),
    };
    let old_str = match args["old_string"].as_str() {
        Some(s) => s,
        None => return "Error: 'old_string' parameter is required".into(),
    };
    let new_str = match args["new_string"].as_str() {
        Some(s) => s,
        None => return "Error: 'new_string' parameter is required".into(),
    };
    let path = match resolve_tool_path(path_str, workspace, "patch_file") {
        Ok(path) => path,
        Err(message) => return message,
    };
    let display_path = path.display_path().to_path_buf();
    let old_str = old_str.to_string();
    let new_str = new_str.to_string();

    let result =
        tokio::task::spawn_blocking(move || patch_checked_workspace_text(path, &old_str, &new_str))
            .await;
    match result {
        Ok(Ok(count)) => format!(
            "Patched {} (replaced 1 of {} occurrences)",
            display_path.display(),
            count
        ),
        Ok(Err(error)) if error == "old_string not found" => format!(
            "patch_file error: old_string not found in {}",
            display_path.display()
        ),
        Ok(Err(error)) => format!("patch_file error: {error}"),
        Err(error) => format!("patch_file error: worker failed: {error}"),
    }
}

// ── list_dir ─────────────────────────────────────────────────────────────────

pub(crate) async fn tool_list_dir(
    args: &serde_json::Value,
    _config: &Config,
    workspace: &Path,
) -> String {
    let path_str = args["path"].as_str().unwrap_or(".");
    let path = match resolve_tool_path_readable(path_str, workspace, "list_dir") {
        Ok(path) => path,
        Err(message) => return message,
    };
    let display_path = path.display_path().to_path_buf();

    match path {
        ReadableToolPath::Virtual(path) => match tokio::fs::read_dir(&path).await {
            Ok(mut entries) => {
                let mut items = Vec::new();
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let name = entry.file_name().to_string_lossy().to_string();
                    match entry.metadata().await {
                        Ok(meta) => {
                            if meta.is_dir() {
                                items.push(format!("  {name}/"));
                            } else {
                                items.push(format!("  {name}  ({})", format_size(meta.len())));
                            }
                        }
                        Err(_) => items.push(format!("  {name}  (?)")),
                    }
                }
                items.sort();
                if items.is_empty() {
                    format!("{} — (empty)", display_path.display())
                } else {
                    format!("{}:\n{}", display_path.display(), items.join("\n"))
                }
            }
            Err(e) => format!("list_dir error: {e}"),
        },
        ReadableToolPath::Workspace(path) => {
            match tokio::task::spawn_blocking(move || list_checked_workspace_directory(path)).await
            {
                Ok(Ok(entries)) => {
                    let mut items = Vec::new();
                    for entry in entries {
                        let name = entry.name.to_string_lossy();
                        match (entry.kind, entry.metadata) {
                            (CheckedWorkspaceDirEntryKind::Directory, _) => {
                                items.push(format!("  {name}/"));
                            }
                            (CheckedWorkspaceDirEntryKind::File, Some(metadata)) => {
                                items.push(format!("  {name}  ({})", format_size(metadata.len())));
                            }
                            _ => items.push(format!("  {name}  (?)")),
                        }
                    }
                    if items.is_empty() {
                        format!("{} — (empty)", display_path.display())
                    } else {
                        format!("{}:\n{}", display_path.display(), items.join("\n"))
                    }
                }
                Ok(Err(error)) => format!("list_dir error: {error}"),
                Err(error) => format!("list_dir error: worker failed: {error}"),
            }
        }
    }
}

// ── search_files ─────────────────────────────────────────────────────────────

async fn collect_file_paths(
    root: &Path,
    file_glob: Option<&str>,
    max_depth: usize,
    max_files: usize,
) -> Vec<PathBuf> {
    let skip_dirs = [
        "node_modules",
        "target",
        ".git",
        "__pycache__",
        "dist",
        "build",
        ".next",
        "vendor",
    ];
    let mut files = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        if depth > max_depth || files.len() >= max_files {
            break;
        }
        let Ok(mut entries) = tokio::fs::read_dir(&dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            if files.len() >= max_files {
                break;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            // Never follow a descendant symlink: the requested search root was
            // workspace-checked, but a nested link could otherwise escape it.
            if file_type.is_symlink() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if file_type.is_dir() {
                if !name.starts_with('.') && !skip_dirs.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if file_type.is_file() {
                if let Some(glob) = file_glob {
                    if matches_glob(&name, glob) {
                        files.push(path);
                    }
                } else {
                    files.push(path);
                }
            }
        }
    }
    files
}

fn collect_checked_file_paths(
    root: CheckedWorkspacePath,
    file_glob: Option<&str>,
    max_depth: usize,
    max_files: usize,
) -> Vec<CheckedWorkspacePath> {
    let skip_dirs = [
        "node_modules",
        "target",
        ".git",
        "__pycache__",
        "dist",
        "build",
        ".next",
        "vendor",
    ];
    let mut files = Vec::new();
    let mut stack = vec![(root, 0_usize)];
    while let Some((directory, depth)) = stack.pop() {
        if depth > max_depth || files.len() >= max_files {
            break;
        }
        let Ok(entries) = directory.read_directory() else {
            continue;
        };
        for entry in entries {
            if files.len() >= max_files {
                break;
            }
            let name = entry.name.to_string_lossy();
            match entry.kind {
                CheckedWorkspaceDirEntryKind::Directory => {
                    if !name.starts_with('.') && !skip_dirs.contains(&name.as_ref()) {
                        stack.push((entry.path, depth + 1));
                    }
                }
                CheckedWorkspaceDirEntryKind::File => {
                    if file_glob.is_none_or(|pattern| matches_glob(&name, pattern)) {
                        files.push(entry.path);
                    }
                }
                CheckedWorkspaceDirEntryKind::LinkOrReparse
                | CheckedWorkspaceDirEntryKind::Other
                | CheckedWorkspaceDirEntryKind::Missing => {}
            }
        }
    }
    files
}

fn search_checked_files(
    root: CheckedWorkspacePath,
    re: &Regex,
    file_glob: Option<&str>,
    max_results: usize,
) -> Vec<String> {
    let files = collect_checked_file_paths(root, file_glob, 5, 10_000);
    let mut results = Vec::new();
    for file_path in files {
        if results.len() >= max_results {
            break;
        }
        let Ok((mut file, _)) = file_path.open_file_for_read() else {
            continue;
        };
        let mut content = String::new();
        if file.read_to_string(&mut content).is_err() {
            continue;
        }
        for (index, line) in content.lines().enumerate() {
            if re.is_match(line) {
                results.push(format!(
                    "{}:{}:{}",
                    file_path.display_path().display(),
                    index + 1,
                    line.trim()
                ));
                if results.len() >= max_results {
                    break;
                }
            }
        }
    }
    results
}

pub(crate) async fn tool_search_files(
    args: &serde_json::Value,
    config: &Config,
    workspace: &Path,
) -> String {
    let pattern_str = match args["pattern"].as_str() {
        Some(p) => p,
        None => return "Error: 'pattern' parameter is required".into(),
    };
    let re = match Regex::new(pattern_str) {
        Ok(r) => r,
        Err(e) => return format!("Invalid regex pattern: {e}"),
    };
    let dir_str = args["path"].as_str().unwrap_or(".");
    let dir = match resolve_tool_path_readable(dir_str, workspace, "search_files") {
        Ok(path) => path,
        Err(message) => return message,
    };
    let display_path = dir.display_path().to_path_buf();
    let file_glob = args["file_glob"].as_str();
    let max_results = args["max_results"].as_u64().unwrap_or(50) as usize;
    if max_results == 0 {
        return "search_files error: max_results must be >= 1".into();
    }

    let mut results: Vec<String> = match dir {
        ReadableToolPath::Virtual(dir) => {
            let files = collect_file_paths(&dir, file_glob, 5, 10_000).await;
            let re = Arc::new(re);
            let found_count = Arc::new(AtomicUsize::new(0));
            let batched_results: Vec<Vec<String>> = stream::iter(files.into_iter())
                .map(|file_path| {
                    let re = Arc::clone(&re);
                    let found_count = Arc::clone(&found_count);
                    async move {
                        if found_count.load(Ordering::Relaxed) >= max_results {
                            return Vec::new();
                        }
                        let Ok(content) = tokio::fs::read_to_string(&file_path).await else {
                            return Vec::new();
                        };
                        let matches: Vec<String> = content
                            .lines()
                            .enumerate()
                            .filter(|(_, line)| re.is_match(line))
                            .map(|(i, line)| {
                                format!("{}:{}:{}", file_path.display(), i + 1, line.trim())
                            })
                            .collect();
                        found_count.fetch_add(matches.len(), Ordering::Relaxed);
                        matches
                    }
                })
                .buffered(32)
                .collect()
                .await;
            batched_results.into_iter().flatten().collect()
        }
        ReadableToolPath::Workspace(dir) => {
            let file_glob = file_glob.map(str::to_string);
            match tokio::task::spawn_blocking(move || {
                search_checked_files(dir, &re, file_glob.as_deref(), max_results)
            })
            .await
            {
                Ok(results) => results,
                Err(error) => return format!("search_files error: worker failed: {error}"),
            }
        }
    };
    results.truncate(max_results);

    if results.is_empty() {
        format!(
            "No matches for '{}' in {}",
            pattern_str,
            display_path.display()
        )
    } else {
        let header = format!("{} matches:\n", results.len());
        truncate(
            &format!("{header}{}", results.join("\n")),
            config.max_output_bytes,
        )
    }
}

// ── delete_file ──────────────────────────────────────────────────────────────

pub(crate) async fn tool_delete_file(args: &serde_json::Value, workspace: &Path) -> String {
    let path_str = match args["path"].as_str() {
        Some(p) => p,
        None => return "Error: 'path' parameter is required".into(),
    };
    let path = match resolve_tool_path(path_str, workspace, "delete_file") {
        Ok(path) => path,
        Err(message) => return message,
    };
    let display_path = path.display_path().to_path_buf();

    match tokio::task::spawn_blocking(move || delete_checked_workspace_file(path)).await {
        Ok(Ok(())) => format!("Deleted {}", display_path.display()),
        Ok(Err(error)) => format!("delete_file error: {error}"),
        Err(error) => format!("delete_file error: worker failed: {error}"),
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    struct TempTree(PathBuf);

    impl TempTree {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "lingclaw-fs-capability-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).expect("create filesystem capability fixture");
            Self(path)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn replace_file_after_resolution(
        workspace: &Path,
        name: &str,
    ) -> (CheckedWorkspacePath, PathBuf, PathBuf) {
        let target = workspace.join(name);
        let moved = workspace.join(format!("moved-{name}"));
        let replacement = workspace.join(format!("replacement-{name}"));
        std::fs::write(&target, b"original").expect("seed original file");
        std::fs::write(&replacement, b"replacement").expect("seed replacement file");
        let checked = resolve_tool_path(name, workspace, "fixture").expect("resolve original file");
        std::fs::rename(&target, &moved).expect("move resolved file");
        std::fs::rename(&replacement, &target).expect("install replacement file");
        (checked, target, moved)
    }

    #[cfg(any(target_os = "linux", target_os = "android", windows))]
    #[test]
    fn filesystem_callers_reject_targets_replaced_after_resolution() {
        let tree = TempTree::new("replacement");
        let workspace = tree.0.join("workspace");
        std::fs::create_dir(&workspace).expect("create workspace");

        let (read, read_target, read_original) =
            replace_file_after_resolution(&workspace, "read.txt");
        assert!(read_checked_workspace_text(read).is_err());
        assert_eq!(
            std::fs::read(&read_target).expect("read replacement"),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(&read_original).expect("read original"),
            b"original"
        );

        let (write, write_target, write_original) =
            replace_file_after_resolution(&workspace, "write.txt");
        assert!(write_checked_workspace_text(write, "changed").is_err());
        assert_eq!(
            std::fs::read(&write_target).expect("read replacement"),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(&write_original).expect("read original"),
            b"original"
        );

        let (patch, patch_target, patch_original) =
            replace_file_after_resolution(&workspace, "patch.txt");
        assert!(patch_checked_workspace_text(patch, "original", "changed").is_err());
        assert_eq!(
            std::fs::read(&patch_target).expect("read replacement"),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(&patch_original).expect("read original"),
            b"original"
        );

        let (delete, delete_target, delete_original) =
            replace_file_after_resolution(&workspace, "delete.txt");
        assert!(delete_checked_workspace_file(delete).is_err());
        assert_eq!(
            std::fs::read(&delete_target).expect("read replacement"),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(&delete_original).expect("read original"),
            b"original"
        );

        let directory = workspace.join("directory");
        let moved_directory = workspace.join("moved-directory");
        std::fs::create_dir(&directory).expect("create original directory");
        std::fs::write(directory.join("inside.txt"), b"inside").expect("seed original directory");
        let checked = resolve_tool_path("directory", &workspace, "fixture")
            .expect("resolve original directory");
        std::fs::rename(&directory, &moved_directory).expect("move resolved directory");
        std::fs::create_dir(&directory).expect("create replacement directory");
        std::fs::write(directory.join("outside.txt"), b"replacement")
            .expect("seed replacement directory");
        assert!(list_checked_workspace_directory(checked).is_err());
        assert_eq!(
            std::fs::read(directory.join("outside.txt")).expect("read replacement entry"),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(moved_directory.join("inside.txt")).expect("read original entry"),
            b"inside"
        );
    }
}
