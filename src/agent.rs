// ══════════════════════════════════════════════════════════════════════════════
//  Agent Phase State Machine
//
//  ReAct-style controlled decision layer. The four phases map to the classic
//  Thought → Action → Observation cycle, but use structured tool calling
//  instead of text-based Action parsing.
//
//      Analyze ──► Act ──► Observe ──► Analyze  (loop)
//                    │                   │
//                    │                   └──► Finish  (exit)
//                    └──► Finish  (no tools)
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
    #[allow(dead_code)] // Phase 3: controls WS phase events
    pub(crate) show_react: bool,
}

impl AgentLoopCtx {
    pub(crate) fn new(show_react: bool) -> Self {
        Self {
            phase: AgentPhase::Analyze,
            cycles: 0,
            tool_calls: 0,
            show_react,
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
    pub(crate) fn transition_to_finish(&mut self) {
        debug_assert_eq!(self.phase, AgentPhase::Analyze, "Finish requires Analyze");
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
        .filter(|r| r.result.len() > OBSERVATION_SUMMARY_THRESHOLD)
        .map(|r| {
            let line_count = r.result.lines().count();
            let byte_size = r.result.len();
            ObservationSummary {
                tool_call_id: r.id.clone(),
                tool_name: r.name.clone(),
                byte_size,
                line_count,
                hint: format!(
                    "{} returned {line_count} lines / {byte_size} bytes — focus on key findings",
                    r.name
                ),
            }
        })
        .collect()
}

/// Build a compact context hint from observation summaries.
/// Injected into the system prompt's trailing section before the next
/// Analyze round so the model knows which tool outputs were large.
/// Returns `None` if no summaries exist.
pub(crate) fn build_observation_context_hint(summaries: &[ObservationSummary]) -> Option<String> {
    if summaries.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(summaries.len() + 1);
    lines.push("## Recent Observation Notes".to_owned());
    for s in summaries {
        lines.push(format!(
            "- **{}** (id: {}): {}",
            s.tool_name, s.tool_call_id, s.hint
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

/// Basic finish check: the model produced a response with content and no
/// tool_calls. Phase 3 will extend with task-type-aware verification.
#[allow(dead_code)] // Phase 3: task-type-aware finish heuristic
pub(crate) fn is_finish(has_content: bool, has_tool_calls: bool) -> bool {
    has_content && !has_tool_calls
}

/// Empty-response finish: no content and no tool_calls.
#[allow(dead_code)] // Phase 3: task-type-aware finish
pub(crate) fn is_empty_finish(has_content: bool, has_tool_calls: bool) -> bool {
    !has_content && !has_tool_calls
}

// ══════════════════════════════════════════════════════════════════════════════
//  Tests
// ══════════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_transitions_happy_path() {
        let mut ctx = AgentLoopCtx::new(false);
        assert_eq!(ctx.phase(), AgentPhase::Analyze);

        // Analyze → Act
        ctx.transition_to_act();
        assert_eq!(ctx.phase(), AgentPhase::Act);

        // Act → Observe (2 tool calls)
        ctx.transition_to_observe(2);
        assert_eq!(ctx.phase(), AgentPhase::Observe);
        assert_eq!(ctx.tool_calls, 2);

        // Observe → Analyze (new cycle)
        ctx.transition_to_analyze();
        assert_eq!(ctx.phase(), AgentPhase::Analyze);
        assert_eq!(ctx.cycles, 1);

        // Analyze → Finish
        ctx.transition_to_finish();
        assert_eq!(ctx.phase(), AgentPhase::Finish);
    }

    #[test]
    fn direct_finish_without_tools() {
        let mut ctx = AgentLoopCtx::new(false);
        assert_eq!(ctx.phase(), AgentPhase::Analyze);
        ctx.transition_to_finish();
        assert_eq!(ctx.phase(), AgentPhase::Finish);
        assert_eq!(ctx.cycles, 0);
        assert_eq!(ctx.tool_calls, 0);
    }

    #[test]
    fn multi_cycle_tracking() {
        let mut ctx = AgentLoopCtx::new(true);
        for i in 0..5 {
            ctx.transition_to_act();
            ctx.transition_to_observe(1);
            assert_eq!(ctx.tool_calls, i + 1);
            ctx.transition_to_analyze();
        }
        assert_eq!(ctx.cycles, 5);
        assert_eq!(ctx.tool_calls, 5);
        ctx.transition_to_finish();
        assert_eq!(ctx.phase(), AgentPhase::Finish);
    }

    #[test]
    #[should_panic(expected = "Act requires Analyze")]
    fn invalid_act_from_observe() {
        let mut ctx = AgentLoopCtx::new(false);
        ctx.transition_to_act();
        ctx.transition_to_observe(1);
        ctx.transition_to_act(); // wrong: should go to Analyze first
    }

    #[test]
    #[should_panic(expected = "Finish requires Analyze")]
    fn invalid_finish_from_act() {
        let mut ctx = AgentLoopCtx::new(false);
        ctx.transition_to_act();
        ctx.transition_to_finish(); // wrong: should be in Analyze
    }

    #[test]
    fn observation_annotation_short() {
        let short = "ok";
        assert_eq!(maybe_annotate_observation("exec", short), "ok");
    }

    #[test]
    fn observation_annotation_long() {
        let long = "x\n".repeat(3000);
        let annotated = maybe_annotate_observation("exec", &long);
        assert!(annotated.starts_with("[Observation: exec returned"));
        assert!(annotated.contains("3000 lines"));
        assert!(annotated.ends_with(&long));
    }

    #[test]
    fn finish_heuristic() {
        assert!(is_finish(true, false));
        assert!(!is_finish(true, true));
        assert!(!is_finish(false, false)); // empty = not a "real" finish
        assert!(is_empty_finish(false, false));
        assert!(!is_empty_finish(true, false));
    }

    #[test]
    fn summarize_observations_empty_when_short() {
        let results = vec![ToolResultEntry {
            id: "c1".into(),
            name: "exec".into(),
            result: "short output".into(),
        }];
        assert!(summarize_observations(&results).is_empty());
    }

    #[test]
    fn summarize_observations_produces_summary_for_large() {
        let big = "x\n".repeat(3000);
        let results = vec![
            ToolResultEntry {
                id: "c1".into(),
                name: "read_file".into(),
                result: big.clone(),
            },
            ToolResultEntry {
                id: "c2".into(),
                name: "exec".into(),
                result: "ok".into(),
            },
        ];
        let summaries = summarize_observations(&results);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].tool_name, "read_file");
        assert_eq!(summaries[0].byte_size, big.len());
        assert!(summaries[0].hint.contains("3000 lines"));
    }

    #[test]
    fn observation_context_hint_none_when_empty() {
        assert!(build_observation_context_hint(&[]).is_none());
    }

    #[test]
    fn observation_context_hint_builds_markdown() {
        let summaries = vec![ObservationSummary {
            tool_call_id: "c1".into(),
            tool_name: "read_file".into(),
            byte_size: 5000,
            line_count: 100,
            hint: "read_file returned 100 lines / 5000 bytes — focus on key findings".into(),
        }];
        let hint = build_observation_context_hint(&summaries).unwrap();
        assert!(hint.starts_with("## Recent Observation Notes"));
        assert!(hint.contains("**read_file**"));
        assert!(hint.contains("c1"));
    }
}
