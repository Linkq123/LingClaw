use chrono::{DateTime, Duration as ChronoDuration, FixedOffset, Local};
use std::collections::{HashMap, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use crate::{ChatMessage, Config, config_dir_path, memory, subagents, tools};

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
    let mut merged = Vec::new();

    let append_layer = |merged: &mut Vec<SkillMeta>, layer: Vec<SkillMeta>| {
        let layer_names = layer
            .iter()
            .map(|skill| skill.name.clone())
            .collect::<std::collections::HashSet<_>>();
        merged.retain(|skill| !layer_names.contains(&skill.name));
        merged.extend(layer);
    };

    // Layer 1: system (bundled with binary)
    if let Some(dir) = system_skills_dir() {
        append_layer(
            &mut merged,
            discover_skills_in_dir(&dir, SkillSource::System, "system://skills/"),
        );
    }

    // Layer 2: global (~/.lingclaw/skills/)
    if let Some(dir) = global_skills_dir() {
        append_layer(
            &mut merged,
            discover_skills_in_dir(&dir, SkillSource::Global, "~/.lingclaw/skills/"),
        );
    }

    // Layer 3: session workspace (skills/)
    let session_dir = workspace.join(SKILLS_DIR);
    append_layer(
        &mut merged,
        discover_skills_in_dir(&session_dir, SkillSource::Session, "skills/"),
    );

    // Cross-layer deduplication: later layers still override earlier layers by
    // skill name, but duplicates within the same layer are preserved. This keeps
    // namespaced system skills such as `anthropics/pdf` and `openai/pdf`
    // independently addressable.
    merged.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    merged
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

fn system_skill_matches_patterns(path: &str, patterns: &HashSet<String>) -> bool {
    let rel_dir = system_skill_relative_dir(path).unwrap_or_else(|| path.to_string());
    for pattern in patterns {
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

/// Check whether a system skill path is disabled by any entry in the disabled set.
///
/// `path` looks like `system://skills/anthropics/pdf/SKILL.md`.
/// `disabled` entries are relative segments like `anthropics` or `anthropics/pdf`.
///
/// A disabled entry matches if it equals the relative dir or is a prefix of it.
pub(crate) fn is_system_skill_disabled(path: &str, disabled: &HashSet<String>) -> bool {
    system_skill_matches_patterns(path, disabled)
}

/// Check whether a system skill path is enabled by any entry in the enabled set.
///
/// This uses the same relative-dir and parent-pattern matching as disabled
/// system skills, so an entry like `anthropics` enables all current
/// `anthropics/...` system skills.
pub(crate) fn is_system_skill_enabled(path: &str, enabled: &HashSet<String>) -> bool {
    system_skill_matches_patterns(path, enabled)
}

/// Extract the relative system skill directory id from a virtual or relative path.
///
/// `system://skills/anthropics/pdf/SKILL.md` → `anthropics/pdf`
/// `anthropics/pdf/SKILL.md` → `anthropics/pdf`
pub(crate) fn system_skill_relative_dir(path: &str) -> Option<String> {
    const PREFIX: &str = "system://skills/";
    let relative = path.strip_prefix(PREFIX).unwrap_or(path);
    let rel_dir = relative.strip_suffix("/SKILL.md").unwrap_or(relative);
    if rel_dir.is_empty() || rel_dir.contains("..") || rel_dir.split('/').any(str::is_empty) {
        return None;
    }
    Some(rel_dir.to_string())
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
        if let Some(content) = read_prompt_nonempty(workspace.join(name)) {
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

// ── Prompt file caches ────────────────────────────────────────────────────────

pub(crate) fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PromptFileFingerprint {
    modified: Option<SystemTime>,
    len: Option<u64>,
}

/// Cheap, read-free signature for a watched prompt file: (mtime, len), both
/// taken from a single `metadata()` call. This is collected once per top-level
/// Analyze cycle, so it must NOT read file contents — doing so re-read every
/// persona/memory file on every cycle (and twice on a cache miss). Adding `len`
/// to mtime catches same-mtime edits that change the file length. The only
/// blind spot is an edit that keeps both mtime and len identical, which is
/// unreachable for real text edits: any save bumps mtime.
fn prompt_file_fingerprint(path: &Path) -> PromptFileFingerprint {
    let metadata = std::fs::metadata(path).ok();
    let modified = metadata.as_ref().and_then(|m| m.modified().ok());
    let len = metadata.as_ref().map(|m| m.len());

    PromptFileFingerprint { modified, len }
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

// ── Persona cache (stable, invalidates on user edits) ─────────────────────────

const PERSONA_WATCH_FILES: &[&str] = &[
    "BOOTSTRAP.md",
    "AGENTS.md",
    "AGENT.md",
    "IDENTITY.md",
    "USER.md",
    "SOUL.md",
];

struct PersonaCache {
    workspace: PathBuf,
    fingerprints: Vec<PromptFileFingerprint>,
    result: String,
}

type PersonaCacheLock = OnceLock<Mutex<Option<PersonaCache>>>;
static PERSONA_CACHE: PersonaCacheLock = OnceLock::new();

fn collect_persona_fingerprints(workspace: &Path) -> Vec<PromptFileFingerprint> {
    PERSONA_WATCH_FILES
        .iter()
        .map(|name| prompt_file_fingerprint(&workspace.join(name)))
        .collect()
}

fn load_persona_uncached(workspace: &Path) -> String {
    let bootstrap = read_prompt_nonempty(workspace.join(BOOTSTRAP_FILE));

    if let Some(bs_content) = bootstrap {
        // Bootstrap mode: first-run identity setup
        let mut parts = vec![format!("<!-- {BOOTSTRAP_FILE} -->\n{bs_content}")];
        if let Some((name, agent)) = read_agent_prompt(workspace) {
            parts.push(format!("<!-- {name} -->\n{agent}"));
        }
        return parts.join("\n\n---\n\n");
    }

    // Normal mode: stable persona files
    let mut parts = Vec::new();
    if let Some((name, content)) = read_agent_prompt(workspace) {
        parts.push(format!("<!-- {name} -->\n{content}"));
    }
    for name in &PERSONA_WATCH_FILES[3..] {
        // IDENTITY.md, USER.md, SOUL.md
        if let Some(content) = read_prompt_nonempty(workspace.join(name)) {
            parts.push(format!("<!-- {name} -->\n{content}"));
        }
    }
    parts.join("\n\n---\n\n")
}

fn load_persona(workspace: &Path) -> String {
    // Fingerprint (mtime + len) every watched file before loading. This is a
    // read-free metadata check: on a cache hit no persona file is read, and on a
    // miss each file is read exactly once (in load_persona_uncached). Collecting
    // the fingerprint *before* the uncached read means an edit landing mid-load
    // yields a stale signature that simply misses on the next call
    // (self-correcting) rather than caching a result keyed to content it was
    // never built from. Same-mtime edits that change length are detected; the
    // identical-mtime-and-identical-len case is an accepted blind spot.
    let fingerprints = collect_persona_fingerprints(workspace);
    let cache = PERSONA_CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref c) = *guard
            && c.workspace == workspace
            && c.fingerprints == fingerprints
        {
            return c.result.clone();
        }
    }
    let result = load_persona_uncached(workspace);
    {
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(PersonaCache {
            workspace: workspace.to_path_buf(),
            fingerprints,
            result: result.clone(),
        });
    }
    result
}

// ── Memory cache (invalidates on memory writes or day rollover) ───────────────

struct MemoryCache {
    workspace: PathBuf,
    today: String,
    fingerprints: Vec<PromptFileFingerprint>,
    result: String,
}

type MemoryCacheLock = OnceLock<Mutex<Option<MemoryCache>>>;
static MEMORY_CACHE: MemoryCacheLock = OnceLock::new();

fn collect_memory_fingerprints(
    workspace: &Path,
    today: &str,
    yesterday: &str,
) -> Vec<PromptFileFingerprint> {
    vec![
        prompt_file_fingerprint(&workspace.join("BOOTSTRAP.md")), // bootstrap gate
        prompt_file_fingerprint(&workspace.join("MEMORY.md")),
        prompt_file_fingerprint(&workspace.join("memory").join(format!("{today}.md"))),
        prompt_file_fingerprint(&workspace.join("memory").join(format!("{yesterday}.md"))),
    ]
}

fn load_memory_uncached(workspace: &Path, today: &str, yesterday: &str) -> String {
    // In bootstrap mode memory is always empty.
    if read_nonempty(workspace.join(BOOTSTRAP_FILE)).is_some() {
        return String::new();
    }

    let mut parts = Vec::new();
    if let Some(content) = read_prompt_nonempty(workspace.join("MEMORY.md")) {
        parts.push(format!("<!-- MEMORY.md -->\n{content}"));
    }

    const DAILY_MEMORY_CHAR_BUDGET: usize = 4000;
    for date_str in &[today, yesterday] {
        let path = workspace.join("memory").join(format!("{date_str}.md"));
        if let Some(content) = read_prompt_nonempty(&path) {
            let content = crate::truncate(&content, DAILY_MEMORY_CHAR_BUDGET);
            parts.push(format!("<!-- memory/{date_str}.md -->\n{content}"));
        }
    }
    parts.join("\n\n---\n\n")
}

fn load_memory(workspace: &Path, today: &str, yesterday: &str) -> String {
    // See load_persona: a read-free (mtime + len) signature collected before the
    // load keeps the cache hit path content-read-free and avoids the double read
    // on a miss. Note the whole daily memory file is signed even though only the
    // first DAILY_MEMORY_CHAR_BUDGET chars are injected, so edits past the
    // truncation point still invalidate the cache.
    let fingerprints = collect_memory_fingerprints(workspace, today, yesterday);
    let cache = MEMORY_CACHE.get_or_init(|| Mutex::new(None));
    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref c) = *guard
            && c.workspace == workspace
            && c.today == today
            && c.fingerprints == fingerprints
        {
            return c.result.clone();
        }
    }
    let result = load_memory_uncached(workspace, today, yesterday);
    {
        let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        *guard = Some(MemoryCache {
            workspace: workspace.to_path_buf(),
            today: today.to_string(),
            fingerprints,
            result: result.clone(),
        });
    }
    result
}

// ── Public interface ──────────────────────────────────────────────────────────

#[derive(Clone)]
pub(crate) struct PromptFiles {
    pub persona: String,
    pub memory: String,
}

pub(crate) fn load_session_prompt_files_with_snapshot(
    workspace: &Path,
    snapshot: LocalTimeSnapshot,
) -> PromptFiles {
    maybe_complete_bootstrap(workspace);

    let today = snapshot.today();
    let yesterday = snapshot.yesterday();

    PromptFiles {
        persona: load_persona(workspace),
        memory: load_memory(workspace, &today, &yesterday),
    }
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

/// Read a prompt file and strip leading YAML frontmatter before injection.
/// The on-disk file is left unchanged; bootstrap baseline comparisons use the
/// raw file content instead.
fn read_prompt_nonempty(path: impl AsRef<Path>) -> Option<String> {
    let path = path.as_ref();
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let trimmed = strip_yaml_frontmatter(&content).trim();
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

fn strip_yaml_frontmatter(content: &str) -> &str {
    let normalized = content.strip_prefix('\u{feff}').unwrap_or(content);
    let Some(after_open) = normalized.strip_prefix("---") else {
        return normalized;
    };

    let Some(after_open) = after_open
        .strip_prefix("\r\n")
        .or_else(|| after_open.strip_prefix('\n'))
    else {
        return normalized;
    };

    let mut offset = normalized.len() - after_open.len();
    let mut frontmatter_lines = Vec::new();
    for line in after_open.split_inclusive('\n') {
        let line_without_newline = line.trim_end_matches(['\r', '\n']);
        offset += line.len();
        if line_without_newline == "---" {
            return if looks_like_yaml_frontmatter(&frontmatter_lines) {
                &normalized[offset..]
            } else {
                normalized
            };
        }
        frontmatter_lines.push(line_without_newline);
    }

    normalized
}

fn looks_like_yaml_frontmatter(lines: &[&str]) -> bool {
    let mut saw_key_value = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with("- ") {
            continue;
        }

        let Some((key, _value)) = trimmed.split_once(':') else {
            return false;
        };
        let key = key.trim();
        if key.is_empty()
            || !key
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
        {
            return false;
        }
        saw_key_value = true;
    }

    saw_key_value
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
    date_time.format("%Y-%m-%d %H:%M %:z").to_string()
}

// ── System Prompt ──────────────────────────────────────────────────────────

static SYSTEM_PROMPT_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static SYSTEM_PROMPT_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);

const SYSTEM_PROMPT_CACHE_MAX_ENTRIES: usize = 64;

type SystemPromptCacheLock =
    OnceLock<std::sync::Mutex<HashMap<SystemPromptStaticCacheKey, String>>>;
static SYSTEM_PROMPT_STATIC_CACHE: SystemPromptCacheLock = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SystemPromptStaticCacheKey {
    workspace: PathBuf,
    query: Option<String>,
    enabled_skills_hash: u64,
    persona_hash: u64,
    behavior_hash: u64,
    tool_lines_hash: u64,
    mcp_note_hash: u64,
    skills_hash: u64,
    agents_hash: u64,
}

const PROMPT_FILE_NOTE: &str = "## Preloaded Prompt Files\n\
These prompt-file contents were already loaded into this system prompt from the session workspace.\n\
Do not call file tools just to verify or re-read BOOTSTRAP.md, AGENTS.md, AGENT.md, IDENTITY.md, USER.md, or SOUL.md when their content is already present below.\n\
Only read those files if the user explicitly asks to inspect them, if you need to edit them, or if a task depends on checking whether the on-disk file has changed.";

const AGENT_BEHAVIOR_SECTION: &str = "## Agent Behavior

You operate in a ReAct loop: **Analyze** the situation, **Act** by calling tools, **Observe** the results, then either loop or **Finish**.

- **Tool strategy:** Prefer calling tools to gather information over speculating. Batch independent read-only calls together. Run write operations one at a time.
- **Error recovery:** When a tool fails, diagnose the cause and try a different approach - different arguments, a different tool, or an alternative path. Do not repeat the same failing call.
- **Delegation:** For complex, self-contained subtasks, delegate to a sub-agent via the `task` tool. Handle simple, quick work yourself.
- **Finishing:** When the task is complete, deliver your result. When you are genuinely stuck with no further options, say so honestly. Do not pad with speculative follow-ups.";

const PLAN_ONLY_AGENT_BEHAVIOR_SECTION: &str = "## Agent Behavior

You operate in plan-only mode: **Analyze** the situation, optionally **Act** with read-only exploration tools, **Observe** the results, then **Finish** with a plan.

- **Tool strategy:** Prefer read-only tools to gather information over speculating. Batch independent read-only calls together.
- **Boundaries:** Do not modify files, run shell commands, update todos, delegate to sub-agents, or claim work has been performed.
- **Finishing:** Deliver a concrete execution plan with affected areas, validation suggestions, and risks or unknowns. Wait for the user to approve execution before making changes.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SystemPromptToolMode {
    Execute,
    PlanOnly,
}

impl SystemPromptToolMode {
    fn agent_behavior_section(self) -> &'static str {
        match self {
            Self::Execute => AGENT_BEHAVIOR_SECTION,
            Self::PlanOnly => PLAN_ONLY_AGENT_BEHAVIOR_SECTION,
        }
    }

    fn is_plan_only(self) -> bool {
        matches!(self, Self::PlanOnly)
    }
}

pub(crate) fn build_system_prompt(
    config: &Config,
    workspace: &Path,
    model: &str,
    enabled_system_skills: &HashSet<String>,
) -> ChatMessage {
    build_system_prompt_with_query_cached(config, workspace, model, enabled_system_skills, None)
}

fn hash_prompt_part<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn hash_enabled_system_skills(enabled_system_skills: &HashSet<String>) -> u64 {
    let mut items: Vec<&str> = enabled_system_skills.iter().map(String::as_str).collect();
    items.sort_unstable();
    hash_prompt_part(&items)
}

#[allow(clippy::too_many_arguments)]
fn build_system_prompt_static_prefix_cached(
    workspace: &Path,
    current_query: Option<&str>,
    enabled_system_skills: &HashSet<String>,
    persona: &str,
    agent_behavior_section: &str,
    tool_lines: &str,
    mcp_note: &str,
    skills_section: &str,
    agents_section: &str,
) -> String {
    let query = current_query
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(ToOwned::to_owned);
    let enabled_skills_hash = hash_enabled_system_skills(enabled_system_skills);
    let persona_hash = hash_prompt_part(&persona);
    let behavior_hash = hash_prompt_part(&agent_behavior_section);
    let tool_lines_hash = hash_prompt_part(&tool_lines);
    let mcp_note_hash = hash_prompt_part(&mcp_note);
    let skills_hash = hash_prompt_part(&skills_section);
    let agents_hash = hash_prompt_part(&agents_section);
    let key = SystemPromptStaticCacheKey {
        workspace: workspace.to_path_buf(),
        query,
        enabled_skills_hash,
        persona_hash,
        behavior_hash,
        tool_lines_hash,
        mcp_note_hash,
        skills_hash,
        agents_hash,
    };
    let cache = SYSTEM_PROMPT_STATIC_CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));

    {
        let guard = cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stable_prefix) = guard.get(&key) {
            SYSTEM_PROMPT_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return stable_prefix.clone();
        }
    }

    SYSTEM_PROMPT_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
    let stable_prefix = format!(
        r#"{persona}

{prompt_file_note}

{agent_behavior_section}

## Available Tools
{tool_lines}{mcp_note}{skills_section}{agents_section}"#,
        persona = persona,
        prompt_file_note = PROMPT_FILE_NOTE,
        agent_behavior_section = agent_behavior_section,
        tool_lines = tool_lines,
        mcp_note = mcp_note,
        skills_section = skills_section,
        agents_section = agents_section,
    );

    let mut guard = cache.lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() >= SYSTEM_PROMPT_CACHE_MAX_ENTRIES {
        guard.clear();
    }
    guard.insert(key, stable_prefix.clone());
    stable_prefix
}

pub(crate) fn system_prompt_cache_metrics() -> (u64, u64) {
    (
        SYSTEM_PROMPT_CACHE_HITS.load(Ordering::Relaxed),
        SYSTEM_PROMPT_CACHE_MISSES.load(Ordering::Relaxed),
    )
}

pub(crate) fn build_system_prompt_with_query_cached(
    config: &Config,
    workspace: &Path,
    model: &str,
    enabled_system_skills: &HashSet<String>,
    current_query: Option<&str>,
) -> ChatMessage {
    build_system_prompt_with_query_cached_for_tool_mode(
        config,
        workspace,
        model,
        enabled_system_skills,
        current_query,
        SystemPromptToolMode::Execute,
    )
}

pub(crate) fn build_system_prompt_with_query_cached_for_tool_mode(
    config: &Config,
    workspace: &Path,
    model: &str,
    enabled_system_skills: &HashSet<String>,
    current_query: Option<&str>,
    tool_mode: SystemPromptToolMode,
) -> ChatMessage {
    let view_image_available = config.s3.is_some() && config.model_supports_image(model);
    build_system_prompt_with_query_cached_for_tool_mode_with_view_image(
        config,
        workspace,
        model,
        enabled_system_skills,
        current_query,
        tool_mode,
        view_image_available,
    )
}

pub(crate) fn build_system_prompt_with_query_cached_for_tool_mode_with_view_image(
    config: &Config,
    workspace: &Path,
    model: &str,
    enabled_system_skills: &HashSet<String>,
    current_query: Option<&str>,
    tool_mode: SystemPromptToolMode,
    view_image_available: bool,
) -> ChatMessage {
    let os_name = if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    let cwd = workspace.display();
    let local_snapshot = current_local_snapshot();
    let local_time = local_snapshot.datetime_label();
    let agent_behavior_section = tool_mode.agent_behavior_section();
    let tool_lines = if tool_mode.is_plan_only() {
        tools::render_read_only_tool_prompt_lines_with_view_image(config, view_image_available)
    } else {
        tools::render_tool_prompt_lines_with_query_and_view_image(
            config,
            current_query,
            view_image_available,
        )
    };
    let prompt_files = load_session_prompt_files_with_snapshot(workspace, local_snapshot);
    let persona = prompt_files.persona;
    let memory_files = prompt_files.memory;
    let mcp_note = if tool_mode.is_plan_only() {
        String::new()
    } else {
        tools::mcp::runtime_tool_note(config, workspace)
            .map(|note| format!("\n\n## MCP Runtime\n- {note}"))
            .unwrap_or_default()
    };

    let skills_section = discover_all_skills(workspace)
        .into_iter()
        .filter(|s| {
            s.source != SkillSource::System
                || is_system_skill_enabled(&s.path, enabled_system_skills)
        })
        .collect::<Vec<_>>();
    let skills_section = render_skills_catalog(&skills_section, current_query)
        .map(|s| format!("\n\n{s}"))
        .unwrap_or_default();

    let structured_memory_section = if config.structured_memory {
        memory::format_memory_for_injection(
            &memory::load_structured_memory(workspace),
            current_query,
        )
        .map(|s| format!("\n\n{s}"))
        .unwrap_or_default()
    } else {
        String::new()
    };

    let agents_section = if tool_mode.is_plan_only() {
        String::new()
    } else {
        let agents = subagents::discovery::discover_all_agents(workspace);
        subagents::render_agents_catalog_with_query(&agents, current_query)
            .map(|s| format!("\n\n{s}"))
            .unwrap_or_default()
    };

    let stable_prefix = build_system_prompt_static_prefix_cached(
        workspace,
        current_query,
        enabled_system_skills,
        &persona,
        agent_behavior_section,
        &tool_lines,
        &mcp_note,
        &skills_section,
        &agents_section,
    );
    let prompt = format!(
        r#"{stable_prefix}

---
## Memory
{memory_files}{structured_memory_section}

## Environment
- OS: {os_name}
- Current system local time: {local_time}
- Working directory: {cwd}
- Model: {model}"#, // The `---\n## Memory\n` prefix above is used as the cache-split
        // delimiter by ENV_BLOCK_DELIMITER in providers.rs - keep them in sync.
        stable_prefix = stable_prefix,
        memory_files = memory_files,
        structured_memory_section = structured_memory_section,
        os_name = os_name,
        local_time = local_time,
        cwd = cwd,
        model = model,
    );

    ChatMessage {
        role: "system".into(),
        content: Some(prompt),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }
}

#[cfg(test)]
#[path = "tests/prompts_tests.rs"]
mod tests;
