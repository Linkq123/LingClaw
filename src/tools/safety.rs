//! Safety helpers for the CLI/Tools layer: dangerous-command detection and
//! workspace-scoped path resolution. These are consumed by the tool
//! implementations in `src/tools/` (and exercised by tests); the HTTP/WS
//! server core in `main.rs` does not call them directly.

use std::path::{Path, PathBuf};

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

pub(crate) fn resolve_path_checked(path_str: &str, workspace: &Path) -> Result<PathBuf, String> {
    let workspace_root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let raw = Path::new(path_str);
    let relative = if raw.is_absolute() {
        if let Ok(relative) = raw.strip_prefix(workspace) {
            relative.to_path_buf()
        } else if let Ok(relative) = raw.strip_prefix(&workspace_root) {
            relative.to_path_buf()
        } else if let Ok(canonical_raw) = raw.canonicalize() {
            canonical_raw
                .strip_prefix(&workspace_root)
                .map(PathBuf::from)
                .map_err(|_| {
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

    if relative.components().any(|component| {
        matches!(component, std::path::Component::Normal(part) if part == ".lingclaw-bootstrap")
    }) {
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
                        .is_some_and(|target| !target.starts_with(&workspace_root));
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
                    && !canon.starts_with(&workspace_root)
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
