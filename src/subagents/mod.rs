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

use serde::{Deserialize, Serialize};

/// Maximum number of concurrent sub-agent tasks in a single Act phase.
#[allow(dead_code)] // Reserved for future parallel task execution
pub(crate) const MAX_CONCURRENT_SUBAGENTS: usize = 3;

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
    /// `task` is always denied to prevent recursive sub-agent spawning.
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        // Never allow recursive task delegation
        if tool_name == "task" {
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
pub(crate) fn render_agents_catalog(agents: &[SubAgentSpec]) -> Option<String> {
    if agents.is_empty() {
        return None;
    }

    let mut lines = Vec::with_capacity(agents.len() + 6);
    lines.push("## Sub-Agents".to_string());
    lines.push(String::new());
    lines.push(
        "Use the `task` tool to delegate work to specialized sub-agents. \
         Each sub-agent runs in an isolated context with its own tool set."
            .to_string(),
    );
    lines.push(String::new());

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

/// Filter the built-in tool specs according to sub-agent permissions.
/// Returns tool names that are allowed for this sub-agent.
pub(crate) fn filter_tools_for_agent(spec: &SubAgentSpec) -> Vec<String> {
    crate::tools::tool_specs()
        .iter()
        .filter(|ts| spec.tools.is_allowed(ts.name))
        .map(|ts| ts.name.to_string())
        .collect()
}

#[cfg(test)]
#[path = "../tests/subagents_tests.rs"]
mod subagents_tests;
