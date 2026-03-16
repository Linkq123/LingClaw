// ══════════════════════════════════════════════════════════════════════════════
//  Agent Phase State Machine
//
//  ReAct-style controlled decision layer. The four phases map to the classic
//  Thought → Action → Observation cycle, but use structured tool calling
//  instead of text-based Action parsing.
//
//      Analyze ──► Act ──► Observe ──► Analyze  (loop)
//                                  └──► Finish  (exit)
//
//  The state machine lives *inside* the existing agent loop in main.rs.
//  It does NOT replace the loop — it annotates each iteration with an
//  explicit phase so the decision path is traceable and auditable.
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
//  Observation summary
// ──────────────────────────────────────────────────────────────────────────────

/// Byte threshold above which tool output gets a prefix summary hint.
/// Original content is always preserved—this only prepends a length note
/// to help the model focus on the output structure.
#[allow(dead_code)] // Reserved for a future non-destructive observation-summary path.
const OBSERVATION_SUMMARY_THRESHOLD: usize = 4096;

/// Annotate a long tool result with a brief header so the model knows the
/// output is large and should focus on key findings.
///
/// Returns the original string untouched if it is short enough.
#[allow(dead_code)] // Reserved for a future non-destructive observation-summary path.
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
//  Finish heuristic (Phase 1: simple; Phase 3 will add task-type awareness)
// ──────────────────────────────────────────────────────────────────────────────

/// Basic finish check: the model produced a response with content and no
/// tool_calls. In Phase 3 this will be extended with task-type-aware
/// verification (code edit → ran tests? Q&A → answered clearly?).
#[allow(dead_code)] // Phase 3: task-type-aware finish heuristic
pub(crate) fn is_finish(has_content: bool, has_tool_calls: bool) -> bool {
    has_content && !has_tool_calls
}

/// Alternative: model produced no content and no tool_calls — treat as
/// implicit finish (empty response).
#[allow(dead_code)] // Phase 3: empty-response finish detection
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
}
