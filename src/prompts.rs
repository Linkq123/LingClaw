use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, Local};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy)]
pub(crate) struct LocalTimeSnapshot {
    now: DateTime<FixedOffset>,
}

impl LocalTimeSnapshot {
    fn from_datetime(now: DateTime<FixedOffset>) -> Self {
        Self { now }
    }

    pub(crate) fn today(self) -> String {
        format_local_date(self.now)
    }

    fn yesterday(self) -> String {
        format_local_date(self.now - ChronoDuration::days(1))
    }

    pub(crate) fn hhmm(self) -> String {
        format_local_hhmm(self.now)
    }

    pub(crate) fn datetime_label(self) -> String {
        format_local_datetime_label(self.now)
    }
}

/// Template files to copy into each new session workspace.
/// Each entry: (filename, compile-time embedded content as fallback).
const TEMPLATE_FILES: &[(&str, &str)] = &[
    (
        "BOOTSTRAP.md",
        include_str!("../docs/reference/templates/BOOTSTRAP.md"),
    ),
    (
        "AGENT.md",
        include_str!("../docs/reference/templates/AGENT.md"),
    ),
    (
        "IDENTITY.md",
        include_str!("../docs/reference/templates/IDENTITY.md"),
    ),
    (
        "SOUL.md",
        include_str!("../docs/reference/templates/SOUL.md"),
    ),
    (
        "USER.md",
        include_str!("../docs/reference/templates/USER.md"),
    ),
    (
        "TOOLS.md",
        include_str!("../docs/reference/templates/TOOLS.md"),
    ),
    (
        "MEMORY.md",
        include_str!("../docs/reference/templates/MEMORY.md"),
    ),
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
        eprintln!(
            "WARNING: failed to create memory dir {}: {e}",
            memory_dir.display()
        );
    }

    write_missing_templates(workspace, true);
}

/// Ensure essential workspace directories exist for an existing session loaded
/// from disk. Recreates missing core templates, but intentionally does NOT
/// re-create BOOTSTRAP.md so bootstrap completion persists across reconnects.
pub(crate) fn ensure_session_workspace(workspace: &Path) {
    let memory_dir = workspace.join("memory");
    if let Err(e) = std::fs::create_dir_all(&memory_dir) {
        eprintln!(
            "WARNING: failed to create memory dir {}: {e}",
            memory_dir.display()
        );
    }

    write_missing_templates(workspace, false);
}

pub(crate) fn load_session_prompt_files_with_snapshot(
    workspace: &Path,
    snapshot: LocalTimeSnapshot,
) -> String {
    let bootstrap = read_nonempty(workspace.join("BOOTSTRAP.md"));

    if let Some(bs_content) = bootstrap {
        // Bootstrap mode: first-run identity setup
        let mut parts = vec![format!("<!-- BOOTSTRAP.md -->\n{bs_content}")];
        if let Some(agent) = read_nonempty(workspace.join("AGENT.md")) {
            parts.push(format!("<!-- AGENT.md -->\n{agent}"));
        }
        return parts.join("\n\n---\n\n");
    }

    // Normal mode: full persona
    let mut parts = Vec::new();
    for name in &["AGENT.md", "IDENTITY.md", "USER.md", "SOUL.md"] {
        if let Some(content) = read_nonempty(workspace.join(name)) {
            parts.push(format!("<!-- {name} -->\n{content}"));
        }
    }

    if let Some(content) = read_nonempty(workspace.join("MEMORY.md")) {
        parts.push(format!("<!-- MEMORY.md -->\n{content}"));
    }

    let today = snapshot.today();
    let yesterday = snapshot.yesterday();
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
/// Returns None if the file doesn't exist, is empty, or the avatar is set to
/// an explicit unset marker such as `none`.
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
        if rest.is_empty() || rest.starts_with('（') || rest.starts_with('(') {
            return None;
        }

        if is_none_avatar_value(rest) || has_inline_none_guidance(rest) {
            return None;
        }

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
    None
}

fn is_none_avatar_value(value: &str) -> bool {
    let normalized = value
        .trim()
        .trim_matches(|ch: char| {
            ch.is_whitespace() || matches!(ch, '。' | '.' | '！' | '!' | '？' | '?')
        })
        .to_ascii_lowercase();

    matches!(
        normalized.as_str(),
        "none" | "null" | "unset" | "not set" | "未设置" | "暂未设置"
    )
}

fn has_inline_none_guidance(value: &str) -> bool {
    let trimmed = value.trim();
    let lowered = trimmed.to_ascii_lowercase();
    ["none", "null", "unset", "not set", "未设置", "暂未设置"]
        .iter()
        .any(|unset_value| {
            let Some(rest) = lowered.strip_prefix(unset_value) else {
                return false;
            };

            let suffix = rest.trim_start();
            !suffix.is_empty() && (suffix.starts_with('（') || suffix.starts_with('('))
        })
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
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&data)
    ))
}

/// Read a file and return its trimmed content if non-empty.
/// Missing files are silently skipped; actual I/O errors are logged.
fn read_nonempty(path: impl AsRef<Path>) -> Option<String> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            eprintln!("WARNING: failed to read {}: {e}", path.display());
            None
        }
    }
}

pub(crate) fn current_local_snapshot() -> LocalTimeSnapshot {
    LocalTimeSnapshot::from_datetime(Local::now().fixed_offset())
}

fn format_local_date(date_time: DateTime<FixedOffset>) -> String {
    date_time.format("%Y-%m-%d").to_string()
}

fn format_local_hhmm(date_time: DateTime<FixedOffset>) -> String {
    date_time.format("%H:%M").to_string()
}

fn format_local_datetime_label(date_time: DateTime<FixedOffset>) -> String {
    date_time.format("%Y-%m-%d %H:%M:%S %:z").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_local_datetime_formatters() {
        let date_time = DateTime::parse_from_rfc3339("2026-03-16T00:05:07+08:00")
            .expect("datetime should parse");

        assert_eq!(format_local_date(date_time), "2026-03-16");
        assert_eq!(format_local_hhmm(date_time), "00:05");
        assert_eq!(
            format_local_datetime_label(date_time),
            "2026-03-16 00:05:07 +08:00"
        );
    }

    #[test]
    fn local_time_snapshot_uses_single_now_across_midnight_boundaries() {
        let snapshot = LocalTimeSnapshot::from_datetime(
            DateTime::parse_from_rfc3339("2026-03-16T00:05:07+08:00")
                .expect("datetime should parse"),
        );

        assert_eq!(snapshot.today(), "2026-03-16");
        assert_eq!(snapshot.yesterday(), "2026-03-15");
        assert_eq!(snapshot.hhmm(), "00:05");
        assert_eq!(snapshot.datetime_label(), "2026-03-16 00:05:07 +08:00");
    }

    #[test]
    fn load_session_prompt_files_uses_same_snapshot_for_today_and_yesterday() {
        let workspace = std::env::temp_dir().join("lingclaw-prompt-snapshot-test");
        let _ = fs::remove_dir_all(&workspace);
        fs::create_dir_all(workspace.join("memory")).expect("memory dir should be created");
        fs::write(workspace.join("AGENT.md"), "agent").expect("agent file should be written");
        fs::write(workspace.join("IDENTITY.md"), "identity")
            .expect("identity file should be written");
        fs::write(workspace.join("USER.md"), "user").expect("user file should be written");
        fs::write(workspace.join("SOUL.md"), "soul").expect("soul file should be written");
        fs::write(workspace.join("memory/2026-03-16.md"), "today memory")
            .expect("today memory should be written");
        fs::write(workspace.join("memory/2026-03-15.md"), "yesterday memory")
            .expect("yesterday memory should be written");

        let snapshot = LocalTimeSnapshot::from_datetime(
            DateTime::parse_from_rfc3339("2026-03-16T00:05:07+08:00")
                .expect("datetime should parse"),
        );
        let loaded = load_session_prompt_files_with_snapshot(&workspace, snapshot);

        assert!(loaded.contains("<!-- memory/2026-03-16.md -->\ntoday memory"));
        assert!(loaded.contains("<!-- memory/2026-03-15.md -->\nyesterday memory"));

        let _ = fs::remove_dir_all(&workspace);
    }

    #[test]
    fn parse_identity_avatar_treats_inline_none_guidance_as_unset() {
        let workspace = std::env::temp_dir().join("lingclaw-avatar-inline-none-test");
        let _ = fs::remove_dir_all(&workspace);
        fs::create_dir_all(&workspace).expect("workspace should be created");
        fs::write(
            workspace.join("IDENTITY.md"),
            "- 头像：none （未设置时填写 none；也可填写工作区相对路径、http(s) URL、data URI）\n",
        )
        .expect("identity file should be written");

        let avatar = parse_identity_avatar(&workspace);

        assert_eq!(avatar, None);

        let _ = fs::remove_dir_all(&workspace);
    }

    #[test]
    fn parse_identity_avatar_keeps_text_that_only_starts_with_none() {
        let workspace = std::env::temp_dir().join("lingclaw-avatar-none-prefix-text-test");
        let _ = fs::remove_dir_all(&workspace);
        fs::create_dir_all(&workspace).expect("workspace should be created");
        fs::write(workspace.join("IDENTITY.md"), "- 头像：none-core\n")
            .expect("identity file should be written");

        let avatar = parse_identity_avatar(&workspace);

        assert_eq!(avatar.as_deref(), Some("none-core"));

        let _ = fs::remove_dir_all(&workspace);
    }

    #[test]
    fn parse_identity_avatar_treats_case_mixed_inline_none_guidance_as_unset() {
        let workspace = std::env::temp_dir().join("lingclaw-avatar-none-mixed-case-test");
        let _ = fs::remove_dir_all(&workspace);
        fs::create_dir_all(&workspace).expect("workspace should be created");
        fs::write(
            workspace.join("IDENTITY.md"),
            "- 头像：None (leave unset)\n",
        )
        .expect("identity file should be written");

        let avatar = parse_identity_avatar(&workspace);

        assert_eq!(avatar, None);

        let _ = fs::remove_dir_all(&workspace);
    }
}
