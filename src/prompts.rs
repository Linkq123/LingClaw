use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Template files to copy into each new session workspace.
/// Each entry: (filename, compile-time embedded content as fallback).
const TEMPLATE_FILES: &[(&str, &str)] = &[
    ("BOOTSTRAP.md", include_str!("../docs/reference/templates/BOOTSTRAP.md")),
    ("AGENT.md",     include_str!("../docs/reference/templates/AGENT.md")),
    ("IDENTITY.md",  include_str!("../docs/reference/templates/IDENTITY.md")),
    ("SOUL.md",      include_str!("../docs/reference/templates/SOUL.md")),
    ("USER.md",      include_str!("../docs/reference/templates/USER.md")),
    ("TOOLS.md",     include_str!("../docs/reference/templates/TOOLS.md")),
    ("MEMORY.md",    include_str!("../docs/reference/templates/MEMORY.md")),
];

fn write_missing_templates(workspace: &Path, include_bootstrap: bool) {
    let tpl_dir = templates_dir(); // None is fine — we have embedded fallback

    for &(name, embedded) in TEMPLATE_FILES {
        if !include_bootstrap && name == "BOOTSTRAP.md" {
            continue;
        }
        let dest = workspace.join(name);
        if dest.exists() {
            continue; // never overwrite user edits
        }
        let content = tpl_dir
            .as_ref()
            .and_then(|dir| std::fs::read_to_string(dir.join(name)).ok())
            .unwrap_or_else(|| embedded.to_string());
        if let Err(e) = std::fs::write(&dest, &content) {
            eprintln!("WARNING: failed to write {}: {e}", dest.display());
        }
    }
}

/// Locate the templates directory on disk (prefer disk over embedded).
fn templates_dir() -> Option<PathBuf> {
    // 1. Relative to executable (production: binary sits at project root or in target/)
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1) {
            let candidate = ancestor.join("docs/reference/templates");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }
    // 2. Relative to CWD (dev mode: `cargo run` from project root)
    let cwd = std::env::current_dir().ok()?;
    let candidate = cwd.join("docs/reference/templates");
    if candidate.is_dir() {
        return Some(candidate);
    }
    None
}

/// Initialize a new session workspace: copy template files (skip existing), create memory/ dir.
///
/// Prefers reading templates from disk (`docs/reference/templates/`); if the
/// directory is not found or a specific file can't be read, falls back to the
/// compile-time embedded copy so the session always starts with a valid set.
pub(crate) fn init_session_prompt_files(workspace: &Path) {
    // Ensure memory/ subdirectory exists
    let memory_dir = workspace.join("memory");
    if let Err(e) = std::fs::create_dir_all(&memory_dir) {
        eprintln!("WARNING: failed to create memory dir {}: {e}", memory_dir.display());
    }

    write_missing_templates(workspace, true);
}

/// Ensure essential workspace directories exist for an existing session loaded
/// from disk. Recreates missing core templates, but intentionally does NOT
/// re-create BOOTSTRAP.md so bootstrap completion persists across reconnects.
pub(crate) fn ensure_session_workspace(workspace: &Path) {
    let memory_dir = workspace.join("memory");
    if let Err(e) = std::fs::create_dir_all(&memory_dir) {
        eprintln!("WARNING: failed to create memory dir {}: {e}", memory_dir.display());
    }

    write_missing_templates(workspace, false);
}

/// Load session context for the system prompt.
///
/// **Bootstrap mode** (BOOTSTRAP.md exists): loads BOOTSTRAP.md + AGENT.md only —
/// this is the first-run "who am I?" flow.
///
/// **Normal mode** (BOOTSTRAP.md deleted): loads AGENT.md + IDENTITY.md +
/// USER.md + SOUL.md, then MEMORY.md and today's/yesterday's daily memory.
///
/// Files that don't exist or are empty are silently skipped.
pub(crate) fn load_session_prompt_files(workspace: &Path) -> String {
    let bootstrap = read_nonempty(workspace.join("BOOTSTRAP.md"));

    if let Some(bs_content) = bootstrap {
        // Bootstrap mode: first-run identity setup
        let mut parts = vec![bs_content];
        if let Some(agent) = read_nonempty(workspace.join("AGENT.md")) {
            parts.push(agent);
        }
        return parts.join("\n\n---\n\n");
    }

    // Normal mode: full persona
    let mut parts = Vec::new();
    for name in &["AGENT.md", "IDENTITY.md", "USER.md", "SOUL.md"] {
        if let Some(content) = read_nonempty(workspace.join(name)) {
            parts.push(content);
        }
    }

    if let Some(content) = read_nonempty(workspace.join("MEMORY.md")) {
        parts.push(content);
    }

    let today = chrono_today();
    let yesterday = chrono_yesterday();
    for date_str in &[today, yesterday] {
        let path = workspace.join("memory").join(format!("{date_str}.md"));
        if let Some(content) = read_nonempty(&path) {
            parts.push(format!("<!-- memory/{date_str}.md -->\n{content}"));
        }
    }

    parts.join("\n\n---\n\n")
}

/// Parse the IDENTITY.md file in the workspace and extract the avatar value.
/// Looks for a line matching `- 头像：<value>` or `- 头像: <value>`.
/// Returns None if the file doesn't exist, is empty, or no avatar is set.
pub(crate) fn parse_identity_avatar(workspace: &Path) -> Option<String> {
    let content = read_nonempty(workspace.join("IDENTITY.md"))?;
    for line in content.lines() {
        let line = line.trim().trim_start_matches('-').trim();
        // Must be exactly "头像：<value>" or "头像: <value>" (colon required immediately)
        let rest = if let Some(r) = line.strip_prefix("头像：") {
            r
        } else if let Some(r) = line.strip_prefix("头像:") {
            r
        } else {
            continue;
        };
        let rest = rest.trim();
        if !rest.is_empty() && !rest.starts_with('（') && !rest.starts_with('(') {
            // http(s) URLs and data URIs pass through directly
            if rest.starts_with("http") || rest.starts_with("data:") {
                return Some(rest.to_string());
            }
            // If it ends with a known image extension, resolve as file path; drop on failure
            if has_image_ext(rest) {
                return resolve_avatar_to_data_uri(workspace, rest);
            }
            // Otherwise treat as text/emoji avatar
            return Some(rest.to_string());
        }
    }
    None
}

fn has_image_ext(s: &str) -> bool {
    let s = s.to_ascii_lowercase();
    ["png", "jpg", "jpeg", "gif", "svg", "webp", "ico"]
        .iter()
        .any(|ext| s.ends_with(&format!(".{ext}")))
}

/// Try to read a workspace-relative image file and return a data URI.
fn resolve_avatar_to_data_uri(workspace: &Path, rel_path: &str) -> Option<String> {
    let full = workspace.join(rel_path).canonicalize().ok()?;
    let ws_canon = workspace.canonicalize().ok()?;
    if !full.starts_with(&ws_canon) {
        return None; // path escapes workspace — reject
    }
    let data = std::fs::read(&full).ok()?;
    let ext = full
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let mime = match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        _ => return None,
    };
    use base64::Engine;
    Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&data)))
}

/// Read a file and return its trimmed content if non-empty.
/// Missing files are silently skipped; actual I/O errors are logged.
fn read_nonempty(path: impl AsRef<Path>) -> Option<String> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("WARNING: failed to read {}: {e}", path.display());
            None
        }
    }
}

/// Return today's date as "YYYY-MM-DD" using system time (no chrono crate needed).
pub(crate) fn chrono_today() -> String {
    // seconds since epoch → days → civil date
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    epoch_secs_to_date(secs)
}

/// Return yesterday's date as "YYYY-MM-DD".
fn chrono_yesterday() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(86400);
    epoch_secs_to_date(secs)
}

/// Convert epoch seconds to "YYYY-MM-DD" (civil calendar, UTC).
fn epoch_secs_to_date(secs: u64) -> String {
    // Algorithm from Howard Hinnant's civil_from_days
    let days = (secs / 86400) as i64;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_epoch_to_date() {
        // 2024-01-01 00:00:00 UTC = 1704067200
        assert_eq!(epoch_secs_to_date(1704067200), "2024-01-01");
        // 2026-03-14 00:00:00 UTC = 1773446400
        assert_eq!(epoch_secs_to_date(1773446400), "2026-03-14");
    }
}
