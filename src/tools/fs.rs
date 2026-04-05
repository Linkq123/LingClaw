use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use futures::stream::{self, StreamExt};

use regex::Regex;

use crate::{Config, format_size, matches_glob, resolve_path_checked, truncate};

fn resolve_tool_path(path_str: &str, workspace: &Path, tool_name: &str) -> Result<PathBuf, String> {
    resolve_path_checked(path_str, workspace)
        .map_err(|message| format!("{tool_name} error: {message}"))
}

/// Like `resolve_tool_path` but also resolves virtual skill paths
/// (`system://skills/...`, `~/.lingclaw/skills/...`) for read-only access.
fn resolve_tool_path_readable(
    path_str: &str,
    workspace: &Path,
    tool_name: &str,
) -> Result<PathBuf, String> {
    // Try virtual skill path first (read-only)
    if let Some(real) = crate::prompts::resolve_skill_path(path_str) {
        return Ok(real);
    }
    resolve_tool_path(path_str, workspace, tool_name)
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

    match tokio::fs::read_to_string(&path).await {
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
                        path.display(),
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
                        path.display(),
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
                    let header = format!("[{} — {} lines]\n", path.display(), total);
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

    if let Some(parent) = path.parent()
        && let Err(e) = tokio::fs::create_dir_all(parent).await
    {
        return format!("write_file error: could not create directories: {e}");
    }

    match tokio::fs::write(&path, content).await {
        Ok(()) => format!("Written {} bytes to {}", content.len(), path.display()),
        Err(e) => format!("write_file error: {e}"),
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

    match tokio::fs::read_to_string(&path).await {
        Ok(content) => {
            let count = content.matches(old_str).count();
            if count == 0 {
                return format!(
                    "patch_file error: old_string not found in {}",
                    path.display()
                );
            }
            let new_content = content.replacen(old_str, new_str, 1);
            match tokio::fs::write(&path, &new_content).await {
                Ok(()) => format!(
                    "Patched {} (replaced 1 of {} occurrences)",
                    path.display(),
                    count
                ),
                Err(e) => format!("patch_file write error: {e}"),
            }
        }
        Err(e) => format!("patch_file read error: {e}"),
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

    match tokio::fs::read_dir(&path).await {
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
                format!("{} — (empty)", path.display())
            } else {
                format!("{}:\n{}", path.display(), items.join("\n"))
            }
        }
        Err(e) => format!("list_dir error: {e}"),
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
            let Ok(meta) = entry.metadata().await else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().to_string();
            if meta.is_dir() {
                if !name.starts_with('.') && !skip_dirs.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if meta.is_file() {
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
    let file_glob = args["file_glob"].as_str();
    let max_results = args["max_results"].as_u64().unwrap_or(50) as usize;
    if max_results == 0 {
        return "search_files error: max_results must be >= 1".into();
    }

    let files = collect_file_paths(&dir, file_glob, 5, 10_000).await;
    let re = Arc::new(re);
    let max_results_limit = max_results;
    let found_count = Arc::new(AtomicUsize::new(0));

    // Concurrent file reads with bounded parallelism, early termination,
    // and stable file order (buffered, not buffer_unordered).
    let batched_results: Vec<Vec<String>> = stream::iter(files.into_iter())
        .map(|file_path| {
            let re = Arc::clone(&re);
            let found_count = Arc::clone(&found_count);
            async move {
                if found_count.load(Ordering::Relaxed) >= max_results_limit {
                    return Vec::new();
                }
                let Ok(content) = tokio::fs::read_to_string(&file_path).await else {
                    return Vec::new();
                };
                let matches: Vec<String> = content
                    .lines()
                    .enumerate()
                    .filter(|(_, line)| re.is_match(line))
                    .map(|(i, line)| format!("{}:{}:{}", file_path.display(), i + 1, line.trim()))
                    .collect();
                found_count.fetch_add(matches.len(), Ordering::Relaxed);
                matches
            }
        })
        .buffered(32)
        .collect()
        .await;

    let mut results: Vec<String> = batched_results.into_iter().flatten().collect();
    results.truncate(max_results_limit);

    if results.is_empty() {
        format!("No matches for '{}' in {}", pattern_str, dir.display())
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

    match tokio::fs::metadata(&path).await {
        Ok(meta) if meta.is_file() => match tokio::fs::remove_file(&path).await {
            Ok(()) => format!("Deleted {}", path.display()),
            Err(e) => format!("delete_file error: {e}"),
        },
        Ok(_) => format!(
            "delete_file error: {} is a directory, not a file",
            path.display()
        ),
        Err(e) => format!("delete_file error: {e}"),
    }
}
