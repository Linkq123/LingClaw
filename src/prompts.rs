use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, Local};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use crate::config_dir_path;

// ── Skills ───────────────────────────────────────────────────────────────────────────────

const SKILLS_DIR: &str = "skills";

/// TTL for discovery caches — safety net for content-only changes to existing
/// files (directory mtime doesn't change when a file inside a subdirectory is
/// modified). Structural changes (new/removed skill or agent directories) are
/// detected instantly via directory mtime comparison.
pub(crate) const DISCOVERY_CACHE_TTL_SECS: u64 = 10;

struct SkillsCacheEntry {
    workspace: PathBuf,
    dir_mtimes: Vec<Option<SystemTime>>,
    cached_at: Instant,
    items: Vec<SkillMeta>,
}

type SkillsCache = OnceLock<Mutex<Option<SkillsCacheEntry>>>;
static SKILLS_CACHE: SkillsCache = OnceLock::new();

/// Force-invalidate the skills discovery cache (e.g. after `/skills-system install|uninstall`).
pub(crate) fn invalidate_skills_cache() {
    if let Some(cache) = SKILLS_CACHE.get() {
        *cache.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SkillSource {
    System,
    Global,
    Session,
}

impl SkillSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            SkillSource::System => "system",
            SkillSource::Global => "global",
            SkillSource::Session => "session",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SkillMeta {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) path: String,
    pub(crate) source: SkillSource,
}

/// Locate the system-bundled skills directory on disk.
/// Mirrors the `templates_dir()` pattern: searches relative to the executable
/// then falls back to CWD for dev mode.
fn system_skills_dir() -> Option<PathBuf> {
    // 1. Search relative to executable (dev mode / cargo-bin layout)
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1) {
            let candidate = ancestor.join("docs/reference/skills");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }
    // 2. Installed location: ~/.lingclaw/system-skills/
    if let Some(dir) = config_dir_path() {
        let candidate = dir.join("system-skills");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    // 3. CWD fallback (dev mode)
    let cwd = std::env::current_dir().ok()?;
    let candidate = cwd.join("docs/reference/skills");
    if candidate.is_dir() {
        return Some(candidate);
    }
    None
}

/// Diagnostic: return the resolved system skills directory path (or None).
pub(crate) fn system_skills_resolved_path() -> Option<PathBuf> {
    system_skills_dir()
}

/// Resolve a virtual skill path to its real filesystem path.
///
/// Recognised prefixes:
///   - `system://skills/...` → resolved via `system_skills_dir()`
///   - `~/.lingclaw/skills/...` → resolved via `global_skills_dir()`
///
/// Returns `None` for session-local `skills/...` paths (handled by the normal
/// workspace-relative resolution) or unknown prefixes.
pub(crate) fn resolve_skill_path(virtual_path: &str) -> Option<PathBuf> {
    const SYSTEM_PREFIX: &str = "system://skills/";
    const SYSTEM_BARE: &str = "system://skills";
    const GLOBAL_PREFIX: &str = "~/.lingclaw/skills/";
    const GLOBAL_BARE: &str = "~/.lingclaw/skills";

    let (relative, base_dir) = if let Some(rel) = virtual_path.strip_prefix(SYSTEM_PREFIX) {
        (rel, system_skills_dir()?)
    } else if virtual_path == SYSTEM_BARE {
        ("", system_skills_dir()?)
    } else if let Some(rel) = virtual_path.strip_prefix(GLOBAL_PREFIX) {
        (rel, global_skills_dir()?)
    } else if virtual_path == GLOBAL_BARE {
        ("", global_skills_dir()?)
    } else {
        return None;
    };

    // Reject path traversal attempts
    if relative.contains("..") {
        return None;
    }

    let full = base_dir.join(relative);
    // Canonicalize and verify the resolved path stays inside the base directory
    let canonical = full.canonicalize().ok()?;
    let canonical_base = base_dir.canonicalize().ok()?;
    if !canonical.starts_with(&canonical_base) {
        return None;
    }
    Some(canonical)
}

/// Global skills directory: `~/.lingclaw/skills/`.
fn global_skills_dir() -> Option<PathBuf> {
    let dir = config_dir_path()?.join(SKILLS_DIR);
    if dir.is_dir() { Some(dir) } else { None }
}

/// Scan a single directory for skill subdirectories containing valid `SKILL.md`.
fn discover_skills_in_dir(dir: &Path, source: SkillSource, path_prefix: &str) -> Vec<SkillMeta> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut skills = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = entry.file_name();
        let dir_name_str = dir_name.to_string_lossy();
        let skill_file = path.join("SKILL.md");
        if let Ok(content) = std::fs::read_to_string(&skill_file) {
            // This directory contains a SKILL.md — treat it as a skill.
            if let Some(mut meta) = parse_skill_frontmatter(&content) {
                meta.path = format!("{path_prefix}{dir_name_str}/SKILL.md");
                meta.source = source;
                skills.push(meta);
            }
        } else {
            // No SKILL.md here — recurse into subdirectory (supports org folders like `anthropics/`).
            let sub_prefix = format!("{path_prefix}{dir_name_str}/");
            skills.extend(discover_skills_in_dir(&path, source, &sub_prefix));
        }
    }
    skills
}

/// Discover skills from all three layers (system → global → session) and merge.
/// Later sources can shadow earlier ones if names collide (session wins over global wins over system).
/// Results are cached and invalidated when source directory mtimes change
/// (immediate for structural changes) or after [`DISCOVERY_CACHE_TTL_SECS`]
/// (safety net for in-place content edits).
pub(crate) fn discover_all_skills(workspace: &Path) -> Vec<SkillMeta> {
    let dir_mtimes = collect_skills_dir_mtimes(workspace);
    let cache = SKILLS_CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref c) = *guard
            && c.workspace == workspace
            && c.dir_mtimes == dir_mtimes
            && c.cached_at.elapsed().as_secs() < DISCOVERY_CACHE_TTL_SECS
        {
            return c.items.clone();
        }
    }
    let result = discover_all_skills_uncached(workspace);
    {
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(SkillsCacheEntry {
            workspace: workspace.to_path_buf(),
            dir_mtimes,
            cached_at: Instant::now(),
            items: result.clone(),
        });
    }
    result
}

/// Collect mtimes of the three skills source directories (including immediate
/// subdirectories) for cache invalidation.  Tracking one level of child dirs
/// ensures that adding a skill/agent inside an existing org folder (e.g.
/// `anthropics/new-skill/`) is detected immediately.
fn collect_skills_dir_mtimes(workspace: &Path) -> Vec<Option<SystemTime>> {
    let mut mtimes = Vec::new();
    if let Some(p) = system_skills_dir() {
        mtimes.extend(collect_dir_tree_mtimes(&p));
    } else {
        mtimes.push(None);
    }
    if let Some(p) = global_skills_dir() {
        mtimes.extend(collect_dir_tree_mtimes(&p));
    } else {
        mtimes.push(None);
    }
    mtimes.extend(collect_dir_tree_mtimes(&workspace.join(SKILLS_DIR)));
    mtimes
}

fn discover_all_skills_uncached(workspace: &Path) -> Vec<SkillMeta> {
    let mut all = Vec::new();

    // Layer 1: system (bundled with binary)
    if let Some(dir) = system_skills_dir() {
        all.extend(discover_skills_in_dir(
            &dir,
            SkillSource::System,
            "system://skills/",
        ));
    }

    // Layer 2: global (~/.lingclaw/skills/)
    if let Some(dir) = global_skills_dir() {
        all.extend(discover_skills_in_dir(
            &dir,
            SkillSource::Global,
            "~/.lingclaw/skills/",
        ));
    }

    // Layer 3: session workspace (skills/)
    let session_dir = workspace.join(SKILLS_DIR);
    all.extend(discover_skills_in_dir(
        &session_dir,
        SkillSource::Session,
        "skills/",
    ));

    // Deduplicate: later source wins (session > global > system)
    let mut seen = std::collections::HashMap::new();
    for (idx, skill) in all.iter().enumerate() {
        seen.insert(skill.name.clone(), idx);
    }
    let mut deduped: Vec<SkillMeta> = seen.into_values().map(|idx| all[idx].clone()).collect();
    deduped.sort_by(|a, b| a.name.cmp(&b.name));
    deduped
}

/// Discover skills from a single source layer.
pub(crate) fn discover_skills_by_source(workspace: &Path, source: SkillSource) -> Vec<SkillMeta> {
    let mut skills = match source {
        SkillSource::System => system_skills_dir()
            .map(|dir| discover_skills_in_dir(&dir, SkillSource::System, "system://skills/"))
            .unwrap_or_default(),
        SkillSource::Global => global_skills_dir()
            .map(|dir| discover_skills_in_dir(&dir, SkillSource::Global, "~/.lingclaw/skills/"))
            .unwrap_or_default(),
        SkillSource::Session => {
            let session_dir = workspace.join(SKILLS_DIR);
            discover_skills_in_dir(&session_dir, SkillSource::Session, "skills/")
        }
    };
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Parse YAML frontmatter from a SKILL.md file.
/// Expects `---` delimited frontmatter with `name:` and `description:` fields.
/// Only single-line values are supported (no YAML multi-line `|` or `>` folding).
fn parse_skill_frontmatter(content: &str) -> Option<SkillMeta> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = &trimmed[3..];
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];

    let mut name = None;
    let mut description = None;
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("name:") {
            name = Some(unquote_yaml_value(val));
        } else if let Some(val) = line.strip_prefix("description:") {
            description = Some(unquote_yaml_value(val));
        }
    }

    Some(SkillMeta {
        name: name.filter(|s| !s.is_empty())?,
        description: description.unwrap_or_default(),
        path: String::new(),
        source: SkillSource::Session, // placeholder — caller overrides
    })
}

fn unquote_yaml_value(val: &str) -> String {
    let val = val.trim();
    if (val.starts_with('"') && val.ends_with('"'))
        || (val.starts_with('\'') && val.ends_with('\''))
    {
        val[1..val.len() - 1].to_string()
    } else {
        val.to_string()
    }
}

/// Render a skill catalog section for injection into the system prompt.
/// Returns `None` if no skills are discovered.
///
/// When `current_query` is provided and there are more than `SKILL_FULL_DISPLAY_THRESHOLD`
/// skills, skills are ranked by keyword relevance to the query. The top matches
/// get full descriptions; the rest are listed by name only to save tokens.
pub(crate) fn render_skills_catalog(
    skills: &[SkillMeta],
    current_query: Option<&str>,
) -> Option<String> {
    if skills.is_empty() {
        return None;
    }

    let mut lines = Vec::with_capacity(skills.len() + 4);
    lines.push("## Skills".to_string());
    lines.push(String::new());
    lines.push(
        "The following skills are installed. \
         When a task matches a skill's description, use `read_file` with the \
         SKILL.md path shown in parentheses (e.g. `system://skills/anthropics/pdf/SKILL.md`) \
         to load the full instructions before proceeding."
            .to_string(),
    );
    lines.push(String::new());

    const SKILL_FULL_DISPLAY_THRESHOLD: usize = 5;
    const SKILL_TOP_N: usize = 3;

    if skills.len() > SKILL_FULL_DISPLAY_THRESHOLD
        && let Some(query) = current_query
    {
        let query_tokens = crate::tokenize_for_matching(query);
        let mut ranked: Vec<(usize, &SkillMeta)> = skills
            .iter()
            .map(|s| (skill_relevance(s, &query_tokens), s))
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0));

        // Only compress when at least one skill actually matches the query.
        // Zero-hit queries fall through to full display for discoverability.
        let max_score = ranked.first().map(|(s, _)| *s).unwrap_or(0);
        if max_score > 0 {
            for (i, (_score, skill)) in ranked.iter().enumerate() {
                let source_tag = skill.source.label();
                if i < SKILL_TOP_N {
                    if skill.description.is_empty() {
                        lines.push(format!(
                            "- **{}** [`{}`] (`{}`)",
                            skill.name, source_tag, skill.path
                        ));
                    } else {
                        lines.push(format!(
                            "- **{}** [`{}`] — {} (`{}`)",
                            skill.name, source_tag, skill.description, skill.path
                        ));
                    }
                } else {
                    lines.push(format!(
                        "- **{}** [`{}`] (`{}`)",
                        skill.name, source_tag, skill.path
                    ));
                }
            }
            return Some(lines.join("\n"));
        }
    }

    // Default: all skills with full descriptions
    for skill in skills {
        let source_tag = skill.source.label();
        if skill.description.is_empty() {
            lines.push(format!(
                "- **{}** [`{}`] (`{}`)",
                skill.name, source_tag, skill.path
            ));
        } else {
            lines.push(format!(
                "- **{}** [`{}`] — {} (`{}`)",
                skill.name, source_tag, skill.description, skill.path
            ));
        }
    }

    Some(lines.join("\n"))
}

/// Score a skill's relevance to the query tokens.
fn skill_relevance(skill: &SkillMeta, query_tokens: &[String]) -> usize {
    if query_tokens.is_empty() {
        return 0;
    }
    let text = format!("{} {}", skill.name, skill.description).to_lowercase();
    query_tokens
        .iter()
        .filter(|t| text.contains(t.as_str()))
        .count()
}

/// Check whether a system skill path is disabled by any entry in the disabled set.
///
/// `path` looks like `system://skills/anthropics/pdf/SKILL.md`.
/// `disabled` entries are relative segments like `anthropics` or `anthropics/pdf`.
///
/// A disabled entry matches if it equals the relative dir or is a prefix of it.
pub(crate) fn is_system_skill_disabled(path: &str, disabled: &HashSet<String>) -> bool {
    const PREFIX: &str = "system://skills/";
    let relative = path.strip_prefix(PREFIX).unwrap_or(path);
    // Strip trailing `/SKILL.md` so we get e.g. `anthropics/pdf`
    let rel_dir = relative.strip_suffix("/SKILL.md").unwrap_or(relative);
    for pattern in disabled {
        if rel_dir == pattern.as_str() {
            return true;
        }
        let mut prefix = String::with_capacity(pattern.len() + 1);
        prefix.push_str(pattern);
        prefix.push('/');
        if rel_dir.starts_with(&prefix) {
            return true;
        }
    }
    false
}

/// List available system skill "groups" (top-level directories) for display.
pub(crate) fn list_system_skill_groups() -> Vec<String> {
    let Some(dir) = system_skills_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut groups: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    groups.sort();
    groups
}

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

    pub(crate) fn yesterday(self) -> String {
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

    // Ensure skills/ subdirectory exists
    let skills_dir = workspace.join(SKILLS_DIR);
    if let Err(e) = std::fs::create_dir_all(&skills_dir) {
        eprintln!(
            "WARNING: failed to create skills dir {}: {e}",
            skills_dir.display()
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

    let skills_dir = workspace.join(SKILLS_DIR);
    if let Err(e) = std::fs::create_dir_all(&skills_dir) {
        eprintln!(
            "WARNING: failed to create skills dir {}: {e}",
            skills_dir.display()
        );
    }

    migrate_legacy_agent_file(workspace);
    write_missing_templates(workspace, false);
    ensure_bootstrap_baselines(workspace);
}

// ── Prompt file mtime cache ──────────────────────────────────────────────────

/// All prompt-relevant filenames (relative to workspace) that affect the output.
const PROMPT_WATCH_FILES: &[&str] = &[
    "BOOTSTRAP.md",
    "AGENTS.md",
    "AGENT.md",
    "IDENTITY.md",
    "USER.md",
    "SOUL.md",
    "TOOLS.md",
    "MEMORY.md",
];

struct PromptCache {
    workspace: PathBuf,
    today: String,
    mtimes: Vec<Option<SystemTime>>,
    result: String,
}

type PromptCacheLock = OnceLock<Mutex<Option<PromptCache>>>;
static PROMPT_FILE_CACHE: PromptCacheLock = OnceLock::new();

pub(crate) fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Collect mtimes for a directory and its immediate subdirectories.
/// Returns root mtime followed by sorted child-directory mtimes so that
/// structural changes one level below the root (e.g. a new skill added
/// inside an existing org folder) are detected immediately.
pub(crate) fn collect_dir_tree_mtimes(dir: &Path) -> Vec<Option<SystemTime>> {
    let root_mtime = file_mtime(dir);
    if root_mtime.is_none() {
        return vec![None];
    }
    let mut mtimes = vec![root_mtime];
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut subdirs: Vec<_> = entries.flatten().filter(|e| e.path().is_dir()).collect();
        subdirs.sort_by_key(|e| e.file_name());
        for entry in subdirs {
            mtimes.push(file_mtime(&entry.path()));
        }
    }
    mtimes
}

fn collect_prompt_mtimes(
    workspace: &Path,
    today: &str,
    yesterday: &str,
) -> Vec<Option<SystemTime>> {
    let mut mtimes = Vec::with_capacity(PROMPT_WATCH_FILES.len() + 2);
    for name in PROMPT_WATCH_FILES {
        mtimes.push(file_mtime(&workspace.join(name)));
    }
    mtimes.push(file_mtime(
        &workspace.join("memory").join(format!("{today}.md")),
    ));
    mtimes.push(file_mtime(
        &workspace.join("memory").join(format!("{yesterday}.md")),
    ));
    mtimes
}

pub(crate) fn load_session_prompt_files_with_snapshot(
    workspace: &Path,
    snapshot: LocalTimeSnapshot,
) -> String {
    maybe_complete_bootstrap(workspace);

    let today = snapshot.today();
    let yesterday = snapshot.yesterday();
    let mtimes = collect_prompt_mtimes(workspace, &today, &yesterday);

    let cache = PROMPT_FILE_CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref c) = *guard
            && c.workspace == workspace
            && c.today == today
            && c.mtimes == mtimes
        {
            return c.result.clone();
        }
    }

    let result = load_prompt_files_uncached(workspace, &today, &yesterday);
    {
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(PromptCache {
            workspace: workspace.to_path_buf(),
            today,
            mtimes,
            result: result.clone(),
        });
    }
    result
}

fn load_prompt_files_uncached(workspace: &Path, today: &str, yesterday: &str) -> String {
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

    for name in &["IDENTITY.md", "USER.md", "SOUL.md", "TOOLS.md"] {
        if let Some(content) = read_nonempty(workspace.join(name)) {
            parts.push(format!("<!-- {name} -->\n{content}"));
        }
    }

    if let Some(content) = read_nonempty(workspace.join("MEMORY.md")) {
        parts.push(format!("<!-- MEMORY.md -->\n{content}"));
    }

    // Daily memory budget: cap each day file to avoid unbounded prompt growth
    // (reflection entries accumulate over a busy day).
    const DAILY_MEMORY_CHAR_BUDGET: usize = 4000;

    for date_str in &[today, yesterday] {
        let path = workspace.join("memory").join(format!("{date_str}.md"));
        if let Some(content) = read_nonempty(&path) {
            let content = crate::truncate(&content, DAILY_MEMORY_CHAR_BUDGET);
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
