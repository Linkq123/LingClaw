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
        "AGENTS.md",
        include_str!("../docs/reference/templates/AGENTS.md"),
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

const PRIMARY_AGENT_FILE: &str = "AGENTS.md";
const LEGACY_AGENT_FILE: &str = "AGENT.md";
const BOOTSTRAP_FILE: &str = "BOOTSTRAP.md";
const BOOTSTRAP_BASELINE_DIR: &str = ".lingclaw-bootstrap";
const BOOTSTRAP_PROFILE_FILES: &[&str] = &["IDENTITY.md", "USER.md"];

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

fn migrate_legacy_agent_file(workspace: &Path) {
    let target = workspace.join(PRIMARY_AGENT_FILE);
    if target.exists() {
        return;
    }

    let legacy = workspace.join(LEGACY_AGENT_FILE);
    if !legacy.exists() {
        return;
    }

    if let Err(e) = std::fs::rename(&legacy, &target) {
        eprintln!(
            "WARNING: failed to migrate {} to {}: {e}",
            legacy.display(),
            target.display()
        );
    }
}

fn read_agent_prompt(workspace: &Path) -> Option<(&'static str, String)> {
    for name in [PRIMARY_AGENT_FILE, LEGACY_AGENT_FILE] {
        if let Some(content) = read_nonempty(workspace.join(name)) {
            return Some((name, content));
        }
    }
    None
}

fn maybe_complete_bootstrap(workspace: &Path) {
    let bootstrap_path = workspace.join(BOOTSTRAP_FILE);
    if !bootstrap_path.exists() {
        return;
    }

    if !profile_file_has_user_edits(workspace, "IDENTITY.md")
        && !profile_file_has_user_edits(workspace, "USER.md")
    {
        return;
    }

    match std::fs::remove_file(&bootstrap_path) {
        Ok(()) => remove_bootstrap_baselines(workspace),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "WARNING: failed to remove {} after bootstrap completion: {e}",
            bootstrap_path.display()
        ),
    }
}

fn profile_file_has_user_edits(workspace: &Path, file_name: &str) -> bool {
    let Ok(content) = std::fs::read_to_string(workspace.join(file_name)) else {
        return false;
    };
    let baseline =
        read_bootstrap_baseline(workspace, file_name).or_else(|| template_file_content(file_name));
    let Some(baseline) = baseline else {
        return false;
    };

    normalize_template_text(&content) != normalize_template_text(&baseline)
}

fn bootstrap_baseline_path(workspace: &Path, file_name: &str) -> PathBuf {
    workspace.join(BOOTSTRAP_BASELINE_DIR).join(file_name)
}

fn read_bootstrap_baseline(workspace: &Path, file_name: &str) -> Option<String> {
    std::fs::read_to_string(bootstrap_baseline_path(workspace, file_name)).ok()
}

fn write_bootstrap_baselines(workspace: &Path) {
    let baseline_dir = workspace.join(BOOTSTRAP_BASELINE_DIR);
    if let Err(e) = std::fs::create_dir_all(&baseline_dir) {
        eprintln!(
            "WARNING: failed to create bootstrap baseline dir {}: {e}",
            baseline_dir.display()
        );
        return;
    }

    for &file_name in BOOTSTRAP_PROFILE_FILES {
        let target = bootstrap_baseline_path(workspace, file_name);
        if target.exists() {
            continue;
        }

        let Some(template) = template_file_content(file_name) else {
            continue;
        };

        if let Err(e) = std::fs::write(&target, template) {
            eprintln!(
                "WARNING: failed to write bootstrap baseline {}: {e}",
                target.display()
            );
        }
    }
}

fn ensure_bootstrap_baselines(workspace: &Path) {
    if !workspace.join(BOOTSTRAP_FILE).exists() {
        return;
    }

    let baseline_dir = workspace.join(BOOTSTRAP_BASELINE_DIR);
    if let Err(e) = std::fs::create_dir_all(&baseline_dir) {
        eprintln!(
            "WARNING: failed to create bootstrap baseline dir {}: {e}",
            baseline_dir.display()
        );
        return;
    }

    for &file_name in BOOTSTRAP_PROFILE_FILES {
        let target = bootstrap_baseline_path(workspace, file_name);
        if target.exists() {
            continue;
        }

        let Some(template) = template_file_content(file_name) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(workspace.join(file_name)) else {
            continue;
        };

        if normalize_template_text(&content) != normalize_template_text(&template) {
            continue;
        }

        if let Err(e) = std::fs::write(&target, template) {
            eprintln!(
                "WARNING: failed to write bootstrap baseline {}: {e}",
                target.display()
            );
        }
    }
}

fn remove_bootstrap_baselines(workspace: &Path) {
    let baseline_dir = workspace.join(BOOTSTRAP_BASELINE_DIR);
    match std::fs::remove_dir_all(&baseline_dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!(
            "WARNING: failed to remove bootstrap baseline dir {}: {e}",
            baseline_dir.display()
        ),
    }
}

fn template_file_content(file_name: &str) -> Option<String> {
    let (_, embedded) = TEMPLATE_FILES.iter().find(|(name, _)| *name == file_name)?;
    Some(
        templates_dir()
            .and_then(|dir| std::fs::read_to_string(dir.join(file_name)).ok())
            .unwrap_or_else(|| (*embedded).to_string()),
    )
}

fn normalize_template_text(content: &str) -> String {
    content.replace("\r\n", "\n").trim().to_string()
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

    migrate_legacy_agent_file(workspace);
    write_missing_templates(workspace, true);
    write_bootstrap_baselines(workspace);
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

    migrate_legacy_agent_file(workspace);
    write_missing_templates(workspace, false);
    ensure_bootstrap_baselines(workspace);
}

pub(crate) fn load_session_prompt_files_with_snapshot(
    workspace: &Path,
    snapshot: LocalTimeSnapshot,
) -> String {
    maybe_complete_bootstrap(workspace);
    let bootstrap = read_nonempty(workspace.join(BOOTSTRAP_FILE));

    if let Some(bs_content) = bootstrap {
        // Bootstrap mode: first-run identity setup
        let mut parts = vec![format!("<!-- {BOOTSTRAP_FILE} -->\n{bs_content}")];
        if let Some((name, agent)) = read_agent_prompt(workspace) {
            parts.push(format!("<!-- {name} -->\n{agent}"));
        }
        return parts.join("\n\n---\n\n");
    }

    // Normal mode: full persona
    let mut parts = Vec::new();
    if let Some((name, content)) = read_agent_prompt(workspace) {
        parts.push(format!("<!-- {name} -->\n{content}"));
    }

    for name in &["IDENTITY.md", "USER.md", "SOUL.md"] {
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
#[path = "tests/prompts_tests.rs"]
mod tests;
