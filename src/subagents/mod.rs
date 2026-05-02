// ══════════════════════════════════════════════════════════════════════════════
//  Sub-Agent Registry & Definitions
//
//  Declarative sub-agent system inspired by DeerFlow (context isolation +
//  parallel execution), OpenCode (Markdown config + tool permissions), and
//  OpenClaw (session-based multi-agent coordination).
//
//  Sub-agents are defined as Markdown files with YAML frontmatter (reusing
//  the SKILL.md format) and discovered from three layers:
//    1. System  — docs/reference/agents/
//    2. Global  — ~/.lingclaw/agents/
//    3. Session — {workspace}/agents/
//
//  Each sub-agent runs in an isolated context with its own message history,
//  filtered tool set, and independent token budget.
// ══════════════════════════════════════════════════════════════════════════════

pub(crate) mod discovery;
pub(crate) mod executor;
pub(crate) mod orchestrator;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const AGENT_FULL_DISPLAY_THRESHOLD: usize = 4;
const AGENT_TOP_N: usize = 3;
const AGENT_RECOMMENDATION_TOP_N: usize = 3;

/// Hard upper limit on sub-agent max_turns (prevents runaway custom agents).
pub(crate) const MAX_AGENT_TURNS: usize = 50;

/// Sub-agent definition parsed from an AGENT.md file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SubAgentSpec {
    /// Unique name (from frontmatter).
    pub name: String,
    /// Human-readable description for the LLM to choose agents.
    pub description: String,
    /// System prompt body (Markdown content after frontmatter).
    pub system_prompt: String,
    /// Maximum ReAct cycles before forced finish.
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    /// Tool permission rules.
    #[serde(default)]
    pub tools: ToolPermissions,
    /// MCP auto-assignment policy.
    /// When set, MCP tools are classified and filtered automatically
    /// instead of relying on allow/deny lists for MCP tool names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_policy: Option<McpPolicy>,
    /// Discovery source.
    #[serde(skip)]
    pub source: AgentSource,
    /// Virtual path to the definition file.
    #[serde(skip)]
    pub path: String,
}

fn default_max_turns() -> usize {
    15
}

/// MCP tool auto-assignment policy.
///
/// When set on a sub-agent spec, MCP tools are filtered by classification
/// heuristic instead of the generic allow/deny list.  Agents without this
/// field fall back to the standard allow/deny list for MCP tool names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum McpPolicy {
    /// Only MCP tools classified as non-mutating (read/query/list/search).
    ReadOnly,
    /// All MCP tools are allowed.
    All,
}

/// Tool allow/deny rules for a sub-agent.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ToolPermissions {
    /// Tools explicitly allowed. Empty = all built-in tools (except `task`).
    #[serde(default)]
    pub allow: Vec<String>,
    /// Tools explicitly denied. Applied after allow.
    #[serde(default)]
    pub deny: Vec<String>,
}

impl ToolPermissions {
    /// Check if a tool name is permitted under this permission set.
    /// `task` and `orchestrate` are always denied to prevent recursive sub-agent spawning.
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        // Never allow recursive task delegation or orchestration from sub-agents
        if tool_name == "task" || tool_name == "orchestrate" {
            return false;
        }
        let in_allow = self.allow.is_empty() || self.allow.iter().any(|t| t == tool_name);
        let in_deny = self.deny.iter().any(|t| t == tool_name);
        in_allow && !in_deny
    }
}

/// Where a sub-agent definition was discovered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AgentSource {
    #[default]
    System,
    Global,
    Session,
}

impl AgentSource {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Global => "global",
            Self::Session => "session",
        }
    }
}

/// Render a sub-agent catalog section for injection into the system prompt.
/// Returns `None` if no agents are discovered.
#[cfg(test)]
pub(crate) fn render_agents_catalog(agents: &[SubAgentSpec]) -> Option<String> {
    render_agents_catalog_with_query(agents, None)
}

pub(crate) fn render_agents_catalog_with_query(
    agents: &[SubAgentSpec],
    current_query: Option<&str>,
) -> Option<String> {
    if agents.is_empty() {
        return None;
    }

    let mut lines = Vec::with_capacity(agents.len() + 6);
    lines.push("## Sub-Agents".to_string());
    lines.push(String::new());
    lines.push(
        "Use the `task` tool to delegate work to a single sub-agent, or \
         the `orchestrate` tool to coordinate a multi-agent workflow with \
         serial and parallel execution. Each sub-agent runs in an isolated \
         context with its own tool set."
            .to_string(),
    );
    lines.push(String::new());

    if agents.len() > AGENT_FULL_DISPLAY_THRESHOLD
        && let Some(query) = current_query
            .map(str::trim)
            .filter(|query| !query.is_empty())
    {
        let query_tokens = crate::tokenize_for_matching(query);
        let mut ranked: Vec<(usize, &SubAgentSpec)> = agents
            .iter()
            .map(|agent| (agent_relevance(agent, &query_tokens), agent))
            .collect();
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));

        if ranked.first().map(|(score, _)| *score).unwrap_or(0) > 0 {
            for (idx, (_score, agent)) in ranked.iter().enumerate() {
                let source_tag = agent.source.label();
                if idx < AGENT_TOP_N && !agent.description.is_empty() {
                    lines.push(format!(
                        "- **{}** [`{}`]: {}",
                        agent.name, source_tag, agent.description
                    ));
                } else {
                    lines.push(format!("- **{}** [`{}`]", agent.name, source_tag));
                }
            }
            return Some(lines.join("\n"));
        }
    }

    for agent in agents {
        let source_tag = agent.source.label();
        if agent.description.is_empty() {
            lines.push(format!("- **{}** [`{}`]", agent.name, source_tag));
        } else {
            lines.push(format!(
                "- **{}** [`{}`]: {}",
                agent.name, source_tag, agent.description
            ));
        }
    }

    Some(lines.join("\n"))
}

pub(crate) fn render_ranked_agent_recommendations(
    agents: &[SubAgentSpec],
    current_query: Option<&str>,
    state: Option<&crate::agent::WorkingState>,
) -> Option<String> {
    let ranked = ranked_agents(agents, current_query, state);
    if ranked.is_empty() {
        return None;
    }

    let mut lines = vec!["## Suggested Sub-Agents".to_string()];
    for (idx, (_score, agent)) in ranked.iter().take(AGENT_RECOMMENDATION_TOP_N).enumerate() {
        let source_tag = agent.source.label();
        if agent.description.is_empty() {
            lines.push(format!(
                "{}. **{}** [`{}`]",
                idx + 1,
                agent.name,
                source_tag
            ));
        } else {
            lines.push(format!(
                "{}. **{}** [`{}`]: {}",
                idx + 1,
                agent.name,
                source_tag,
                agent.description
            ));
        }
    }
    Some(lines.join("\n"))
}

pub(crate) fn render_delegation_guidance(
    agents: &[SubAgentSpec],
    current_query: Option<&str>,
    state: &crate::agent::WorkingState,
) -> Option<String> {
    if agents.is_empty() || state.ready_to_finish {
        return None;
    }

    let open_questions = state
        .open_questions
        .iter()
        .filter(|item| !item.trim().is_empty())
        .count();
    let next_actions = state
        .next_actions
        .iter()
        .filter(|item| !item.trim().is_empty())
        .count();
    let blocking_topics = state
        .uncertainties
        .iter()
        .filter(|item| item.blocking)
        .filter_map(|item| {
            let topic = item.topic.trim();
            (!topic.is_empty()).then_some(topic.to_ascii_lowercase())
        })
        .collect::<HashSet<_>>()
        .len();
    let work_tracks = distinct_work_tracks(state);
    let work_track_count = work_tracks.len();
    let action_oriented = !matches!(state.intent, crate::agent::TaskIntent::Inform);
    let goal_looks_multi_track = current_query
        .into_iter()
        .chain(state.primary_goal.as_deref())
        .any(text_looks_multi_track);
    let prefer_orchestrate =
        action_oriented && agents.len() >= 2 && (blocking_topics >= 2 || work_track_count >= 2);
    let prefer_task = !prefer_orchestrate
        && (state.has_blocking_uncertainty() || next_actions > 0 || open_questions > 0);
    if !prefer_task && !prefer_orchestrate {
        return None;
    }

    let ranked = ranked_agents(agents, current_query, Some(state));
    let mut lines = vec!["## Delegation Guidance".to_string()];
    if prefer_orchestrate {
        lines.push(
            "- Prefer `orchestrate` if you want to split the remaining work into multiple independent or dependent strands."
                .to_string(),
        );
    } else {
        lines.push(
            "- Prefer `task` if one focused delegated sub-problem would unblock progress faster than continuing locally."
                .to_string(),
        );
    }
    if state.has_blocking_uncertainty() {
        lines.push(
            "- A blocking uncertainty is still open, so the delegated task should aim to resolve that blocker rather than restating the current status."
                .to_string(),
        );
    }
    if work_track_count >= 2 {
        lines.push(format!(
            "- Distinct remaining work tracks detected: {}.",
            work_track_count
        ));
    } else if goal_looks_multi_track {
        lines.push(
            "- The user goal itself looks multi-part, so keep delegated work scoped to one strand unless you truly need fan-out."
                .to_string(),
        );
    }
    if !ranked.is_empty() {
        let suggested = ranked
            .iter()
            .take(2)
            .map(|(_, agent)| format!("`{}`", agent.name))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("- Likely best-fit agents now: {suggested}"));
    }

    let handoff_items = work_tracks
        .iter()
        .take(2)
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !handoff_items.is_empty() {
        lines.push("- Candidate handoffs:".to_string());
        for item in handoff_items {
            lines.push(format!("  - {item}"));
        }
    }

    Some(lines.join("\n"))
}

fn text_looks_multi_track(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        " and ",
        " as well as ",
        " meanwhile ",
        "同时",
        "并且",
        "以及",
        "分别",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn distinct_work_tracks(state: &crate::agent::WorkingState) -> Vec<String> {
    #[derive(Default)]
    struct WorkTrack {
        representative: String,
        tokens: HashSet<String>,
    }

    let candidates = state
        .uncertainties
        .iter()
        .filter(|item| item.blocking)
        .filter_map(|item| {
            let combined = format!("{} {}", item.topic.trim(), item.reason.trim())
                .trim()
                .to_string();
            (!combined.is_empty()).then_some(combined)
        })
        .chain(state.open_questions.iter().cloned())
        .chain(state.next_actions.iter().cloned())
        .collect::<Vec<_>>();

    let mut tracks: Vec<WorkTrack> = Vec::new();
    for candidate in candidates {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            continue;
        }
        let tokens = work_track_tokens(trimmed);
        if tokens.is_empty() {
            continue;
        }
        if let Some(existing) = tracks
            .iter_mut()
            .find(|track| work_track_tokens_overlap(&track.tokens, &tokens))
        {
            existing.tokens.extend(tokens);
            continue;
        }
        tracks.push(WorkTrack {
            representative: trimmed.to_string(),
            tokens,
        });
    }

    tracks
        .into_iter()
        .map(|track| track.representative)
        .collect()
}

fn work_track_tokens(text: &str) -> HashSet<String> {
    crate::tokenize_for_matching(&text.to_ascii_lowercase())
        .into_iter()
        .filter(|token| is_high_signal_work_track_token(token))
        .collect()
}

fn is_high_signal_work_track_token(token: &str) -> bool {
    (token.len() >= 4 || token.chars().any(|ch| !ch.is_ascii()))
        && !matches!(
            token,
            "this"
                | "that"
                | "these"
                | "those"
                | "with"
                | "from"
                | "into"
                | "onto"
                | "before"
                | "after"
                | "while"
                | "what"
                | "when"
                | "where"
                | "which"
                | "should"
                | "could"
                | "would"
                | "retry"
                | "retried"
                | "replace"
                | "replaced"
                | "relevant"
                | "smaller"
                | "inspect"
                | "first"
                | "files"
                | "file"
                | "path"
                | "paths"
                | "command"
                | "commands"
                | "again"
                | "继续"
                | "接着"
                | "下一步"
                | "接下来"
        )
}

fn work_track_tokens_overlap(left: &HashSet<String>, right: &HashSet<String>) -> bool {
    left.iter().filter(|token| right.contains(*token)).count() >= 2
}

fn agent_relevance(agent: &SubAgentSpec, query_tokens: &[String]) -> usize {
    if query_tokens.is_empty() {
        return 0;
    }
    let text = format!("{} {}", agent.name, agent.description).to_lowercase();
    query_tokens
        .iter()
        .filter(|token| !token.is_empty() && text.contains(token.as_str()))
        .count()
}

fn ranked_agents<'a>(
    agents: &'a [SubAgentSpec],
    current_query: Option<&str>,
    state: Option<&crate::agent::WorkingState>,
) -> Vec<(usize, &'a SubAgentSpec)> {
    let query = build_agent_query(current_query, state);
    let query_tokens = query
        .as_deref()
        .map(crate::tokenize_for_matching)
        .unwrap_or_default();
    if query_tokens.is_empty() {
        return Vec::new();
    }

    let intent = state.map(|state| state.intent);
    let mut ranked: Vec<(usize, &SubAgentSpec)> = agents
        .iter()
        .map(|agent| {
            (
                agent_relevance(agent, &query_tokens) + agent_intent_boost(agent, intent),
                agent,
            )
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    if ranked.first().map(|(score, _)| *score).unwrap_or(0) == 0 {
        return Vec::new();
    }
    ranked
}

fn build_agent_query(
    current_query: Option<&str>,
    state: Option<&crate::agent::WorkingState>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(query) = current_query
        .map(str::trim)
        .filter(|query| !query.is_empty())
    {
        parts.push(query.to_string());
    }
    if let Some(state) = state {
        if let Some(goal) = state.primary_goal.as_deref()
            && !goal.trim().is_empty()
        {
            parts.push(goal.trim().to_string());
        }
        parts.extend(
            state
                .open_questions
                .iter()
                .rev()
                .map(|item| item.trim())
                .filter(|item| !item.is_empty())
                .take(2)
                .map(str::to_string),
        );
        parts.extend(
            state
                .next_actions
                .iter()
                .rev()
                .map(|item| item.trim())
                .filter(|item| !item.is_empty())
                .take(2)
                .map(str::to_string),
        );
        parts.extend(
            state
                .uncertainties
                .iter()
                .rev()
                .filter(|item| item.blocking)
                .take(2)
                .flat_map(|item| [item.topic.trim(), item.reason.trim()])
                .filter(|item| !item.is_empty())
                .map(str::to_string),
        );
    }

    let combined = parts.join(" ");
    let combined = combined.trim();
    if combined.is_empty() {
        None
    } else {
        Some(combined.to_string())
    }
}

fn agent_intent_boost(agent: &SubAgentSpec, intent: Option<crate::agent::TaskIntent>) -> usize {
    let Some(intent) = intent else {
        return 0;
    };
    let text = format!("{} {}", agent.name, agent.description).to_ascii_lowercase();
    let keywords: &[&str] = match intent {
        crate::agent::TaskIntent::Inform => &["docs", "writer", "explain", "research"],
        crate::agent::TaskIntent::Change => &[
            "code",
            "coder",
            "implement",
            "fix",
            "patch",
            "refactor",
            "edit",
        ],
        crate::agent::TaskIntent::Investigate => &[
            "review",
            "debug",
            "investigate",
            "analy",
            "inspect",
            "trace",
        ],
        crate::agent::TaskIntent::Execute => &[
            "run",
            "ops",
            "deploy",
            "benchmark",
            "build",
            "release",
            "test",
        ],
    };
    if keywords.iter().any(|keyword| text.contains(keyword)) {
        3
    } else {
        0
    }
}

/// Filter the built-in tool specs according to sub-agent permissions.
/// Returns tool names that are allowed for this sub-agent.
/// Only includes built-in tools (no MCP). Use `filter_tools_for_agent_with_mcp`
/// when MCP tool names should be included.
pub(crate) fn filter_tools_for_agent(spec: &SubAgentSpec) -> Vec<String> {
    crate::tools::tool_specs()
        .iter()
        .filter(|ts| spec.tools.is_allowed(ts.name))
        .map(|ts| ts.name.to_string())
        .collect()
}

/// Classify whether an MCP tool is likely read-only based on name/description heuristics.
///
/// Splits the tool name (on `_`, `.`, `-`, `/`) and description into words,
/// then checks for mutation indicators.  Conservative: if uncertain, the tool
/// is treated as mutating (i.e. not read-only).
pub(crate) fn is_mcp_tool_read_only(descriptor: &crate::tools::mcp::McpToolDescriptor) -> bool {
    crate::tools::mcp::is_read_only_tool_descriptor(descriptor)
}

/// Filter both built-in and cached MCP tools according to sub-agent permissions.
/// Returns tool names that are allowed for this sub-agent.
///
/// MCP tool filtering depends on `mcp_policy`:
/// - `Some(McpPolicy::All)` — all MCP tools (deny list still applies).
/// - `Some(McpPolicy::ReadOnly)` — only MCP tools classified as non-mutating
///   by `is_mcp_tool_read_only()` (deny list still applies).
/// - `None` — fall back to the generic allow/deny list (`is_allowed()`).
pub(crate) fn filter_tools_for_agent_with_mcp(
    spec: &SubAgentSpec,
    config: &crate::Config,
    workspace: &std::path::Path,
) -> Vec<String> {
    let mut allowed = filter_tools_for_agent(spec);

    // Add MCP tools from cache, filtered according to policy.
    for descriptor in crate::tools::mcp::cached_list_tools(config, workspace) {
        let mcp_ok = match spec.mcp_policy {
            Some(McpPolicy::All) => true,
            Some(McpPolicy::ReadOnly) => is_mcp_tool_read_only(&descriptor),
            None => spec.tools.is_allowed(&descriptor.exposed_name),
        };
        // Deny list is always an override, even with mcp_policy.
        let denied = spec
            .tools
            .deny
            .iter()
            .any(|d| d == &descriptor.exposed_name);
        if mcp_ok && !denied {
            allowed.push(descriptor.exposed_name);
        }
    }

    allowed
}

#[cfg(test)]
#[path = "../tests/subagents_tests.rs"]
mod subagents_tests;
