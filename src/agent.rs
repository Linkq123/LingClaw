// ══════════════════════════════════════════════════════════════════════════════
//  Agent Phase State Machine
//
//  ReAct-style controlled decision layer. The four phases map to the classic
//  Thought → Action → Observation cycle, but use structured tool calling
//  instead of text-based Action parsing.
//
//      Analyze ──► Act ──► Observe ──► Analyze  (loop)
//         │                               │
//         └──────────► Finish ◄───────────┘
//                   (no tools)      (no further tools)
//
//  Phase 2: the agent loop in main.rs uses `match react_ctx.phase()` to
//  drive each iteration — one phase per arm. Inter-phase data flows via
//  local variables (`pending_tool_calls`, `collected_results`, etc.).
// ══════════════════════════════════════════════════════════════════════════════

use serde::{Deserialize, Serialize};

/// The four phases of the agent's ReAct-style decision cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AgentPhase {
    /// Analyze the user request or latest observation.
    /// The model decides whether to call tools or respond directly.
    Analyze,
    /// Execute one or more tool calls issued by the model.
    Act,
    /// Digest tool results: summarize long outputs, update understanding.
    Observe,
    /// Task is complete — the model has produced a final response with no
    /// pending tool calls.
    Finish,
}

impl AgentPhase {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Analyze => "analyze",
            Self::Act => "act",
            Self::Observe => "observe",
            Self::Finish => "finish",
        }
    }
}

impl std::fmt::Display for AgentPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Why the agent loop terminated normally.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FinishReason {
    /// Model produced content with no pending tool calls — normal completion.
    Complete,
    /// Model produced no content and no tool calls — unusual empty response.
    Empty,
}

impl FinishReason {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Empty => "empty",
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Round-level state tracker
// ──────────────────────────────────────────────────────────────────────────────

/// Tracks the agent's phase transitions within a single user turn.
/// Created at the start of each agent loop, consumed at loop exit.
#[derive(Debug)]
pub(crate) struct AgentLoopCtx {
    /// Current phase.
    phase: AgentPhase,
    /// Number of completed Analyze→Act→Observe cycles.
    pub(crate) cycles: usize,
    /// Total tool calls executed in this turn.
    pub(crate) tool_calls: usize,
    /// Whether the ReAct visibility is enabled (controls WS events).
    pub(crate) show_react: bool,
    /// Why the loop finished (set by `transition_to_finish`).
    pub(crate) finish_reason: Option<FinishReason>,
}

impl AgentLoopCtx {
    pub(crate) fn new(show_react: bool) -> Self {
        Self {
            phase: AgentPhase::Analyze,
            cycles: 0,
            tool_calls: 0,
            show_react,
            finish_reason: None,
        }
    }

    pub(crate) fn phase(&self) -> AgentPhase {
        self.phase
    }

    // ── Transitions ──────────────────────────────────────────────────────

    /// Transition: Analyze → Act (model issued tool_calls).
    pub(crate) fn transition_to_act(&mut self) {
        debug_assert_eq!(self.phase, AgentPhase::Analyze, "Act requires Analyze");
        self.phase = AgentPhase::Act;
    }

    /// Transition: Act → Observe (all tool calls executed).
    pub(crate) fn transition_to_observe(&mut self, tool_count: usize) {
        debug_assert_eq!(self.phase, AgentPhase::Act, "Observe requires Act");
        self.tool_calls += tool_count;
        self.phase = AgentPhase::Observe;
    }

    /// Transition: Observe → Analyze (more work needed, next round).
    pub(crate) fn transition_to_analyze(&mut self) {
        debug_assert_eq!(
            self.phase,
            AgentPhase::Observe,
            "Analyze cycle requires Observe"
        );
        self.cycles += 1;
        self.phase = AgentPhase::Analyze;
    }

    /// Transition: Analyze → Finish (model responded without tool_calls).
    pub(crate) fn transition_to_finish(&mut self, reason: FinishReason) {
        debug_assert_eq!(self.phase, AgentPhase::Analyze, "Finish requires Analyze");
        self.finish_reason = Some(reason);
        self.phase = AgentPhase::Finish;
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Observation summary (non-destructive)
// ──────────────────────────────────────────────────────────────────────────────

/// Byte threshold above which tool output triggers an observation summary.
/// Raw tool results are never mutated — summaries are produced as separate
/// WS events and optional context hints for the next Analyze round.
const OBSERVATION_SUMMARY_THRESHOLD: usize = 4096;

/// Lightweight entry for a collected tool result, passed from Act → Observe.
#[derive(Clone, Debug)]
pub(crate) struct ToolResultEntry {
    pub id: String,
    pub name: String,
    pub result: String,
    pub duration_ms: u64,
    pub is_error: bool,
}

/// Non-destructive summary of a large tool result.
#[derive(Clone, Debug)]
pub(crate) struct ObservationSummary {
    pub tool_call_id: String,
    pub tool_name: String,
    pub byte_size: usize,
    pub line_count: usize,
    pub hint: String,
}

/// Generate non-destructive observation summaries for large tool results.
/// Raw results are never touched — this only produces metadata + hints.
pub(crate) fn summarize_observations(results: &[ToolResultEntry]) -> Vec<ObservationSummary> {
    results
        .iter()
        .filter(|r| r.result.len() > OBSERVATION_SUMMARY_THRESHOLD || r.is_error)
        .map(|r| {
            let line_count = r.result.lines().count();
            let byte_size = r.result.len();
            let status = if r.is_error { "FAILED" } else { "ok" };
            ObservationSummary {
                tool_call_id: r.id.clone(),
                tool_name: r.name.clone(),
                byte_size,
                line_count,
                hint: format!(
                    "{} [{status}, {}ms] returned {line_count} lines / {byte_size} bytes{}",
                    r.name,
                    r.duration_ms,
                    if r.is_error {
                        " — error occurred, review output"
                    } else {
                        " — focus on key findings"
                    },
                ),
            }
        })
        .collect()
}

/// Build a compact context hint from observation summaries.
/// Injected into the system prompt's trailing section before the next
/// Analyze round so the model knows which tool outputs were large.
/// When `consecutive_errors` >= 2, appends a degradation hint nudging
/// the model to try alternative approaches instead of retrying the same tool.
/// Returns `None` if no summaries exist and no degradation hint is needed.
pub(crate) fn build_observation_context_hint(
    summaries: &[ObservationSummary],
    consecutive_errors: usize,
) -> Option<String> {
    if summaries.is_empty() && consecutive_errors < 2 {
        return None;
    }
    let mut lines = Vec::with_capacity(summaries.len() + 3);
    lines.push("## Recent Observation Notes".to_owned());
    for s in summaries {
        lines.push(format!(
            "- **{}** (id: {}): {}",
            s.tool_name, s.tool_call_id, s.hint
        ));
    }
    if consecutive_errors >= 3 {
        lines.push(String::new());
        lines.push(format!(
            "⚠ **{consecutive_errors} consecutive tool errors detected.** \
             The current approach is not working. Stop retrying the same tool/arguments. \
             Consider: (1) a completely different tool, (2) different parameters, \
             (3) breaking the task into smaller steps, or (4) asking the user for clarification."
        ));
    } else if consecutive_errors >= 2 {
        lines.push(String::new());
        lines.push(format!(
            "⚠ **{consecutive_errors} consecutive tool errors.** \
             Consider trying an alternative approach or different parameters \
             before retrying."
        ));
    }
    Some(lines.join("\n"))
}

/// Annotate a long tool result with a brief header so the model knows the
/// output is large and should focus on key findings.
///
/// Returns the original string untouched if it is short enough.
/// NOTE: This must NOT be used on the persistence path — only for
/// generating display or context-injection copies.
#[allow(dead_code)] // Phase 3: used for context-injection copies
pub(crate) fn maybe_annotate_observation(tool_name: &str, result: &str) -> String {
    if result.len() <= OBSERVATION_SUMMARY_THRESHOLD {
        return result.to_owned();
    }

    let lines = result.lines().count();
    let bytes = result.len();
    format!(
        "[Observation: {tool_name} returned {lines} lines / {bytes} bytes — \
         focus on key findings]\n{result}"
    )
}

// ──────────────────────────────────────────────────────────────────────────────
//  Finish heuristic
// ──────────────────────────────────────────────────────────────────────────────

/// Basic finish check: the model produced content with no tool_calls.
#[cfg(test)]
pub(crate) fn is_finish(has_content: bool, has_tool_calls: bool) -> bool {
    has_content && !has_tool_calls
}

/// Empty-response finish: no content and no tool_calls.
#[cfg(test)]
pub(crate) fn is_empty_finish(has_content: bool, has_tool_calls: bool) -> bool {
    !has_content && !has_tool_calls
}

/// Evaluate model response and decide whether to finish or continue.
/// Returns `Some(reason)` if the loop should finish, `None` if tool
/// calls are pending and the loop should continue to Act.
pub(crate) fn evaluate_finish(has_content: bool, has_tool_calls: bool) -> Option<FinishReason> {
    if has_tool_calls {
        return None;
    }
    if has_content {
        Some(FinishReason::Complete)
    } else {
        Some(FinishReason::Empty)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
//  Hook System — lifecycle extension points
// ──────────────────────────────────────────────────────────────────────────────

/// Extension points in the agent loop lifecycle.
///
/// Hooks fire at well-defined phase boundaries. Concrete hook implementations
/// (trait + registry) live in `src/main.rs` where they have access to session
/// types, config, and the HTTP client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(dead_code)] // Variants used by hook implementors, not always constructed in core.
pub(crate) enum HookPoint {
    /// Before each Analyze phase — context compression, prompt injection.
    BeforeAnalyze,
    /// After Observe completes — post-processing, metrics.
    AfterObserve,
    /// Agent loop finished — cleanup, final logging.
    OnFinish,
    /// Before a tool is executed — can modify args or reject execution.
    BeforeToolExec,
    /// After a tool completes — can modify the result.
    AfterToolExec,
    /// Before the LLM call — can inject system prompt or override think level.
    BeforeLlmCall,
    /// After a chat command completes — post-execution observation.
    OnCommand,
}

impl HookPoint {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::BeforeAnalyze => "before_analyze",
            Self::AfterObserve => "after_observe",
            Self::OnFinish => "on_finish",
            Self::BeforeToolExec => "before_tool_exec",
            Self::AfterToolExec => "after_tool_exec",
            Self::BeforeLlmCall => "before_llm_call",
            Self::OnCommand => "on_command",
        }
    }
}

impl std::fmt::Display for HookPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Result of a hook execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum HookResult {
    /// Continue normal flow.
    Continue,
}

/// Compute effective think level when session mode is "auto".
/// Adapts reasoning budget based on cycle depth, observation context,
/// user message complexity, and consecutive tool errors.
/// Called only for auto-mode sessions with reasoning-capable models.
///
/// `user_msg_chars` is the **character** count (not byte length) of the
/// latest user message, so CJK text is not unfairly penalised.
pub(crate) fn auto_think_level(
    cycles: usize,
    has_observation: bool,
    user_msg_chars: usize,
    consecutive_errors: usize,
) -> &'static str {
    // Consecutive tool failures: escalate to deeper thinking
    if consecutive_errors >= 2 {
        return "high";
    }

    // Complex user request on first cycle: start with higher budget
    if cycles == 0 && user_msg_chars > 200 {
        return "high";
    }

    match (cycles, has_observation) {
        (0, _) => "medium",
        (_, true) if cycles <= 5 => "high",
        (1..=5, false) => "medium",
        // Efficiency mode for deep loops
        _ => "low",
    }
}

/// Build a soft finish nudge when the agent has been looping for many cycles.
/// Returns `None` for short runs. The nudge is injected into the system prompt
/// to gently guide the model toward wrapping up, preventing runaway loops.
pub(crate) fn build_finish_nudge(cycles: usize) -> Option<&'static str> {
    match cycles {
        0..=14 => None,
        15..=29 => Some(
            "## Guidance\n\
             You have been working for many cycles. Consider whether you have enough \
             information to provide a comprehensive answer. If so, wrap up your response.",
        ),
        _ => Some(
            "## Priority: Wrap Up Now\n\
             You have been working for an extended number of cycles. Provide your best \
             answer with the information gathered so far. Do not start new tool calls \
             unless absolutely critical to answering the user's question.",
        ),
    }
}

/// Heuristic: returns `true` when the query is simple enough to use
/// a cheaper/faster model (when configured). Only relevant on cycle 0.
///
/// A query is considered "simple" when it is short and does not contain
/// keywords suggesting code generation, analysis, or multi-step reasoning.
pub(crate) fn is_simple_query(query: &str) -> bool {
    // Use char count (not byte length) so CJK text isn't unfairly penalised.
    const MAX_SIMPLE_CHARS: usize = 120;
    if query.chars().count() > MAX_SIMPLE_CHARS {
        return false;
    }
    let lower = query.to_ascii_lowercase();
    const COMPLEX_KEYWORDS: &[&str] = &[
        "code",
        "implement",
        "refactor",
        "debug",
        "fix",
        "error",
        "bug",
        "function",
        "class",
        "struct",
        "async",
        "trait",
        "module",
        "explain",
        "analyze",
        "compare",
        "design",
        "architect",
        "write",
        "create",
        "build",
        "generate",
        "convert",
        "```",
        "fn ",
        "def ",
        "import ",
        "use ",
        // Chinese equivalents for common complex-task keywords
        "代码",
        "实现",
        "重构",
        "调试",
        "修复",
        "错误",
        "函数",
        "分析",
        "解释",
        "设计",
        "编写",
        "创建",
        "生成",
    ];
    !COMPLEX_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

// ══════════════════════════════════════════════════════════════════════════════
//  Tests
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
#[path = "tests/agent_tests.rs"]
mod tests;
