// ══════════════════════════════════════════════════════════════════════════════
//  Sub-Agent Discovery
//
//  Three-layer discovery (mirrors the skill system):
//    1. System  — docs/reference/agents/   (bundled with binary)
//    2. Global  — ~/.lingclaw/agents/      (user-wide)
//    3. Session — {workspace}/agents/      (per-session)
//
//  Each agent is defined in an AGENT.md file with YAML frontmatter.
// ══════════════════════════════════════════════════════════════════════════════

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::{AgentSource, SubAgentSpec, ToolPermissions};

const AGENTS_DIR: &str = "agents";
const AGENT_FILE: &str = "AGENT.md";

/// Locate the system-bundled agents directory on disk.
fn system_agents_dir() -> Option<PathBuf> {
    // 1. Search relative to executable
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1) {
            let candidate = ancestor.join("docs/reference/agents");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }
    // 2. Installed location: ~/.lingclaw/system-agents/
    if let Some(dir) = crate::config_dir_path() {
        let candidate = dir.join("system-agents");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    // 3. CWD fallback (dev mode)
    let cwd = std::env::current_dir().ok()?;
    let candidate = cwd.join("docs/reference/agents");
    if candidate.is_dir() {
        return Some(candidate);
    }
    None
}

/// Global agents directory: `~/.lingclaw/agents/`.
fn global_agents_dir() -> Option<PathBuf> {
    let dir = crate::config_dir_path()?.join(AGENTS_DIR);
    if dir.is_dir() { Some(dir) } else { None }
}

/// Scan a single directory for agent subdirectories containing valid `AGENT.md`.
fn discover_agents_in_dir(dir: &Path, source: AgentSource, path_prefix: &str) -> Vec<SubAgentSpec> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut agents = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = entry.file_name();
        let dir_name_str = dir_name.to_string_lossy();
        let agent_file = path.join(AGENT_FILE);
        if let Ok(content) = std::fs::read_to_string(&agent_file) {
            if let Some(mut spec) = parse_agent_frontmatter(&content) {
                spec.source = source;
                spec.path = format!("{path_prefix}{dir_name_str}/{AGENT_FILE}");
                agents.push(spec);
            }
        } else {
            // No AGENT.md — recurse (supports org folders like DeerFlow's builtins/)
            let sub_prefix = format!("{path_prefix}{dir_name_str}/");
            agents.extend(discover_agents_in_dir(&path, source, &sub_prefix));
        }
    }
    agents
}

/// Discover sub-agents from all three layers and merge.
/// Later sources shadow earlier ones on name collision (session > global > system).
pub(crate) fn discover_all_agents(workspace: &Path) -> Vec<SubAgentSpec> {
    let mut all = Vec::new();

    // Layer 1: system (bundled)
    if let Some(dir) = system_agents_dir() {
        all.extend(discover_agents_in_dir(
            &dir,
            AgentSource::System,
            "system://agents/",
        ));
    }

    // Layer 2: global (~/.lingclaw/agents/)
    if let Some(dir) = global_agents_dir() {
        all.extend(discover_agents_in_dir(
            &dir,
            AgentSource::Global,
            "~/.lingclaw/agents/",
        ));
    }

    // Layer 3: session workspace (agents/)
    let session_dir = workspace.join(AGENTS_DIR);
    all.extend(discover_agents_in_dir(
        &session_dir,
        AgentSource::Session,
        "agents/",
    ));

    // Deduplicate: later source wins
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (idx, agent) in all.iter().enumerate() {
        seen.insert(agent.name.clone(), idx);
    }
    let mut deduped: Vec<SubAgentSpec> = seen.into_values().map(|idx| all[idx].clone()).collect();
    deduped.sort_by(|a, b| a.name.cmp(&b.name));
    deduped
}

/// Find a specific sub-agent by name.
pub(crate) fn find_agent(workspace: &Path, name: &str) -> Option<SubAgentSpec> {
    discover_all_agents(workspace)
        .into_iter()
        .find(|a| a.name == name)
}

/// Parse YAML frontmatter from an AGENT.md file.
///
/// Expected format:
/// ```markdown
/// ---
/// name: agent-name
/// description: "What this agent does"
/// max_turns: 15
/// tools:
///   allow: [read_file, search_files, list_dir]
///   deny: [exec]
/// ---
///
/// System prompt body goes here...
/// ```
fn parse_agent_frontmatter(content: &str) -> Option<SubAgentSpec> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return None;
    }
    let rest = &trimmed[3..];
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    let body = rest[end + 4..].trim().to_string();

    let mut name = None;
    let mut description = None;
    let mut max_turns = None;
    let mut allow_tools = Vec::new();
    let mut deny_tools = Vec::new();

    // Track whether we're inside the tools section
    let mut in_tools = false;
    let mut in_allow = false;
    let mut in_deny = false;

    for line in frontmatter.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.is_empty() {
            continue;
        }

        // Detect top-level keys (not indented in raw line)
        let is_top_level = !line.starts_with(' ') && !line.starts_with('\t');

        if is_top_level {
            in_allow = false;
            in_deny = false;

            if let Some(val) = trimmed_line.strip_prefix("name:") {
                name = Some(unquote_yaml_value(val));
                in_tools = false;
            } else if let Some(val) = trimmed_line.strip_prefix("description:") {
                description = Some(unquote_yaml_value(val));
                in_tools = false;
            } else if let Some(val) = trimmed_line.strip_prefix("max_turns:") {
                max_turns = val.trim().parse().ok();
                in_tools = false;
            } else if trimmed_line.starts_with("tools:") {
                in_tools = true;
            }
            continue;
        }

        if !in_tools {
            continue;
        }

        // Handle tools sub-keys (indented lines under `tools:`)
        if let Some(val) = trimmed_line.strip_prefix("allow:") {
            let val = val.trim();
            if val.starts_with('[') {
                allow_tools = parse_inline_list(val);
                in_allow = false;
            } else {
                in_allow = true;
                in_deny = false;
            }
        } else if let Some(val) = trimmed_line.strip_prefix("deny:") {
            let val = val.trim();
            if val.starts_with('[') {
                deny_tools = parse_inline_list(val);
                in_deny = false;
            } else {
                in_deny = true;
                in_allow = false;
            }
        } else if let Some(item_val) = trimmed_line.strip_prefix("- ") {
            let item = item_val.trim().to_string();
            if in_allow {
                allow_tools.push(item);
            } else if in_deny {
                deny_tools.push(item);
            }
        }
    }

    Some(SubAgentSpec {
        name: name.filter(|s| !s.is_empty())?,
        description: description.unwrap_or_default(),
        system_prompt: body,
        max_turns: max_turns.unwrap_or(15),
        tools: ToolPermissions {
            allow: allow_tools,
            deny: deny_tools,
        },
        source: AgentSource::System, // placeholder — caller overrides
        path: String::new(),
    })
}

/// Parse an inline YAML list: `[item1, item2, item3]`.
fn parse_inline_list(val: &str) -> Vec<String> {
    let inner = val
        .trim()
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(val);
    inner
        .split(',')
        .map(unquote_yaml_value)
        .filter(|s| !s.is_empty())
        .collect()
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

/// Test-only accessor for `parse_agent_frontmatter`.
#[cfg(test)]
pub(crate) fn parse_agent_frontmatter_for_test(content: &str) -> Option<SubAgentSpec> {
    parse_agent_frontmatter(content)
}
