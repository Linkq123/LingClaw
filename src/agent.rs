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
const TASK_STATE_PROMPT_CHAR_BUDGET: usize = 1_200;
const TASK_STATE_MAX_COMPLETED_STEPS: usize = 5;
const TASK_STATE_MAX_EVIDENCE_ITEMS: usize = 5;
const TASK_STATE_MAX_OPEN_QUESTIONS: usize = 3;
const TASK_STATE_MAX_NEXT_ACTIONS: usize = 3;
const WORKING_STATE_MAX_ITEMS: usize = 8;
const WORKING_STATE_MAX_TEXT_CHARS: usize = 220;
const RULE_BASED_EVIDENCE_PER_TOOL: usize = 2;

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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum TaskIntent {
    #[default]
    Inform,
    Change,
    Investigate,
    Execute,
}

impl TaskIntent {
    pub(crate) fn classify(query: Option<&str>) -> Self {
        let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) else {
            return Self::Inform;
        };
        let lower = query.to_ascii_lowercase();

        if contains_any(
            &lower,
            &[
                "implement",
                "fix",
                "modify",
                "edit",
                "update",
                "refactor",
                "optimize",
                "rewrite",
                "patch",
                "apply the change",
                "apply the changes",
                "实现",
                "修复",
                "修改",
                "更新",
                "重构",
                "优化",
            ],
        ) {
            return Self::Change;
        }

        if contains_any(
            &lower,
            &[
                "run ",
                "execute",
                "install",
                "launch",
                "start ",
                "stop ",
                "build",
                "compile",
                "deploy",
                "benchmark",
                "profile",
                "运行",
                "执行",
                "安装",
                "启动",
                "编译",
            ],
        ) {
            return Self::Execute;
        }

        if contains_any(
            &lower,
            &[
                "diagnose",
                "investigate",
                "debug",
                "review",
                "inspect",
                "analyze",
                "trace",
                "why is",
                "why does",
                "what caused",
                "诊断",
                "排查",
                "调查",
                "分析",
                "审查",
                "检查",
            ],
        ) {
            return Self::Investigate;
        }

        Self::Inform
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Inform => "inform",
            Self::Change => "change",
            Self::Investigate => "investigate",
            Self::Execute => "execute",
        }
    }

    fn is_action_oriented(self) -> bool {
        !matches!(self, Self::Inform)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum EvidenceConfidence {
    Low,
    #[default]
    Medium,
    High,
}

impl EvidenceConfidence {
    fn is_confirmed(self) -> bool {
        matches!(self, Self::Medium | Self::High)
    }

    fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EvidenceItem {
    #[serde(default)]
    pub claim: String,
    #[serde(default)]
    pub source_tool: String,
    #[serde(default)]
    pub source_ref: String,
    #[serde(default)]
    pub confidence: EvidenceConfidence,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UncertaintyItem {
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub blocking: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StateDigestDelta {
    #[serde(default)]
    pub completed_steps: Vec<String>,
    #[serde(default)]
    pub evidence_add: Vec<EvidenceItem>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    #[serde(default)]
    pub uncertainties_add: Vec<UncertaintyItem>,
    #[serde(default)]
    pub next_actions: Vec<String>,
    #[serde(default)]
    pub ready_to_finish: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WorkingState {
    #[serde(default)]
    pub intent: TaskIntent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_goal: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub completed_steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_questions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uncertainties: Vec<UncertaintyItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
    #[serde(default)]
    pub ready_to_finish: bool,
    #[serde(skip)]
    last_seeded_query: Option<String>,
    #[serde(skip)]
    successful_execution_observed: bool,
    #[serde(skip)]
    successful_change_observed: bool,
}

impl WorkingState {
    /// Seed state from the latest user query.
    ///
    /// Returns `true` when the query redirects the agent to a new goal and the
    /// task state is reset before reseeding.
    pub(crate) fn seed_from_query(&mut self, query: Option<&str>) -> bool {
        let Some(query) =
            query.and_then(|query| sanitize_state_text(query, WORKING_STATE_MAX_TEXT_CHARS))
        else {
            return false;
        };
        if self.last_seeded_query.as_deref() == Some(query.as_str()) {
            return false;
        }
        if self.last_seeded_query.is_some() && query_is_follow_up_continuation(&query) {
            return false;
        }
        let represents_new_goal = self
            .last_seeded_query
            .as_deref()
            .is_some_and(|previous| query_represents_new_goal(previous, &query));
        if represents_new_goal {
            self.reset_for_new_goal();
        }
        self.intent = TaskIntent::classify(Some(&query));
        self.primary_goal = Some(query.clone());
        self.last_seeded_query = Some(query);
        self.recompute_ready_to_finish();
        represents_new_goal
    }

    pub(crate) fn has_blocking_uncertainty(&self) -> bool {
        self.uncertainties.iter().any(|item| item.blocking)
    }

    pub(crate) fn has_confirmed_evidence(&self) -> bool {
        self.evidence
            .iter()
            .any(|item| item.confidence.is_confirmed())
    }

    pub(crate) fn has_successful_execution_trace(&self) -> bool {
        self.successful_execution_observed
            || self
                .completed_steps
                .iter()
                .any(|step| completed_step_counts_as_execution_progress(step))
    }

    pub(crate) fn has_successful_change_trace(&self) -> bool {
        self.successful_change_observed
            || self
                .completed_steps
                .iter()
                .any(|step| completed_step_counts_as_change_progress(step))
    }

    pub(crate) fn recompute_ready_to_finish(&mut self) {
        let has_blocker = self.has_blocking_uncertainty();
        let has_evidence = self.has_confirmed_evidence();
        let has_execution_trace = self.has_successful_execution_trace();
        let has_change_trace = self.has_successful_change_trace();

        self.ready_to_finish = match self.intent {
            TaskIntent::Inform => !has_blocker,
            TaskIntent::Change => !has_blocker && has_change_trace,
            TaskIntent::Investigate => !has_blocker && (has_evidence || has_execution_trace),
            TaskIntent::Execute => !has_blocker && has_execution_trace,
        };
    }

    fn reset_for_new_goal(&mut self) {
        self.completed_steps.clear();
        self.evidence.clear();
        self.open_questions.clear();
        self.uncertainties.clear();
        self.next_actions.clear();
        self.ready_to_finish = false;
        self.successful_execution_observed = false;
        self.successful_change_observed = false;
    }
}

#[cfg(test)]
pub(crate) fn apply_rule_based_working_state_update(
    state: &mut WorkingState,
    results: &[ToolResultEntry],
) {
    apply_rule_based_working_state_update_with_memory(state, results, None);
}

pub(crate) fn apply_rule_based_working_state_update_with_memory(
    state: &mut WorkingState,
    results: &[ToolResultEntry],
    task_memory: Option<&crate::memory::RetrievedTaskMemory>,
) {
    for result in results {
        if result.is_error {
            record_tool_failure(state, result);
            continue;
        }

        if result_counts_as_execution_progress(result) {
            state.successful_execution_observed = true;
        }
        if result_counts_as_change_progress(result) {
            state.successful_change_observed = true;
        }
        if let Some(step) = completed_step_for_result(result) {
            push_unique_text(
                &mut state.completed_steps,
                step,
                WORKING_STATE_MAX_ITEMS,
                WORKING_STATE_MAX_TEXT_CHARS,
            );
        }
        for evidence in evidence_from_result(result) {
            push_unique_evidence(&mut state.evidence, evidence, WORKING_STATE_MAX_ITEMS);
        }
    }

    if let Some(task_memory) = task_memory {
        reconcile_results_with_task_memory(state, results, task_memory);
    }

    state.recompute_ready_to_finish();
}

pub(crate) fn should_trigger_state_digest(results: &[ToolResultEntry]) -> bool {
    results.len() > 2
        || results
            .iter()
            .any(|result| result.is_error || result.result.len() > OBSERVATION_SUMMARY_THRESHOLD)
}

pub(crate) fn merge_state_digest_delta(state: &mut WorkingState, delta: StateDigestDelta) {
    let StateDigestDelta {
        completed_steps,
        evidence_add,
        open_questions,
        uncertainties_add,
        next_actions,
        ready_to_finish: _llm_ready_to_finish,
    } = delta;

    for step in completed_steps {
        push_unique_text(
            &mut state.completed_steps,
            step,
            WORKING_STATE_MAX_ITEMS,
            WORKING_STATE_MAX_TEXT_CHARS,
        );
    }
    for evidence in evidence_add {
        push_unique_evidence(&mut state.evidence, evidence, WORKING_STATE_MAX_ITEMS);
    }
    for question in open_questions {
        push_unique_text(
            &mut state.open_questions,
            question,
            WORKING_STATE_MAX_ITEMS,
            WORKING_STATE_MAX_TEXT_CHARS,
        );
    }
    for uncertainty in uncertainties_add {
        push_unique_uncertainty(
            &mut state.uncertainties,
            uncertainty,
            WORKING_STATE_MAX_ITEMS,
        );
    }
    for action in next_actions {
        push_unique_text(
            &mut state.next_actions,
            action,
            WORKING_STATE_MAX_ITEMS,
            WORKING_STATE_MAX_TEXT_CHARS,
        );
    }

    // Readiness is derived from the merged state itself. The LLM can contribute
    // evidence, questions, and uncertainties, but it does not get to bypass the
    // runtime's finish heuristic with an optimistic flag.
    state.recompute_ready_to_finish();
}

pub(crate) fn render_task_state_for_prompt(state: &WorkingState) -> Option<String> {
    if state.primary_goal.is_none()
        && state.completed_steps.is_empty()
        && state.evidence.is_empty()
        && state.open_questions.is_empty()
        && state.next_actions.is_empty()
    {
        return None;
    }

    let mut lines = vec!["## Task State".to_string()];
    if let Some(goal) = state.primary_goal.as_ref() {
        lines.push(format!("- Goal: {goal}"));
    }
    if !state.completed_steps.is_empty() {
        lines.push("- Completed:".to_string());
        for item in state
            .completed_steps
            .iter()
            .take(TASK_STATE_MAX_COMPLETED_STEPS)
        {
            lines.push(format!("  - {item}"));
        }
    }
    if !state.evidence.is_empty() {
        lines.push("- Evidence:".to_string());
        for item in state.evidence.iter().take(TASK_STATE_MAX_EVIDENCE_ITEMS) {
            lines.push(format!(
                "  - [{}] {} ({})",
                item.confidence.label(),
                item.claim,
                compact_evidence_ref(item)
            ));
        }
    }
    if !state.open_questions.is_empty() {
        lines.push("- Open Questions:".to_string());
        for item in state
            .open_questions
            .iter()
            .take(TASK_STATE_MAX_OPEN_QUESTIONS)
        {
            lines.push(format!("  - {item}"));
        }
    }
    if !state.next_actions.is_empty() {
        lines.push("- Next Actions:".to_string());
        for item in state.next_actions.iter().take(TASK_STATE_MAX_NEXT_ACTIONS) {
            lines.push(format!("  - {item}"));
        }
    }

    let rendered = lines.join("\n");
    if rendered.len() <= TASK_STATE_PROMPT_CHAR_BUDGET {
        return Some(rendered);
    }

    let marker = "\n*(task state truncated)*";
    let keep = TASK_STATE_PROMPT_CHAR_BUDGET.saturating_sub(marker.len());
    Some(format!("{}{}", crate::truncate(&rendered, keep), marker))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TaskPlan {
    pub goal: String,
    pub intent: String,
    pub steps: Vec<TaskPlanStep>,
    pub open_questions: Vec<String>,
    pub suggested_tools: Vec<TaskPlanToolSuggestion>,
    pub suggested_agents: Vec<TaskPlanAgentSuggestion>,
    pub verification_suggestions: Vec<TaskPlanVerificationSuggestion>,
    pub acceptance_criteria: Vec<String>,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskPlanStep {
    pub id: String,
    pub title: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskPlanToolSuggestion {
    pub name: String,
    pub reason: String,
    pub score: usize,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskPlanAgentSuggestion {
    pub name: String,
    pub reason: String,
    pub score: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TaskPlanVerificationSuggestion {
    pub command: String,
    pub reason: String,
    pub confidence: String,
    pub when: String,
}

const TASK_PLAN_PROMPT_CHAR_BUDGET: usize = 1_600;
const TASK_PLAN_MAX_OPEN_QUESTIONS: usize = 3;
const TASK_PLAN_MAX_TOOL_SUGGESTIONS: usize = 6;
const TASK_PLAN_MAX_AGENT_SUGGESTIONS: usize = 3;
const TASK_PLAN_MAX_VERIFICATION_SUGGESTIONS: usize = 5;

pub(crate) fn build_task_plan(
    state: &WorkingState,
    current_query: Option<&str>,
    available_tools: &[String],
    available_agents: &[String],
    recent_tool_history: &[ToolResultEntry],
) -> TaskPlan {
    let goal = state
        .primary_goal
        .clone()
        .or_else(|| current_query.and_then(|query| sanitize_state_text(query, 180)))
        .unwrap_or_else(|| "Respond to the current user request".to_string());
    let intent = state.intent;
    let mut plan = TaskPlan {
        goal,
        intent: task_intent_label(intent).to_string(),
        steps: task_plan_steps(state),
        open_questions: task_plan_open_questions(state),
        suggested_tools: task_plan_tool_suggestions(
            intent,
            current_query,
            available_tools,
            recent_tool_history,
        ),
        suggested_agents: task_plan_agent_suggestions(intent, current_query, available_agents),
        verification_suggestions: task_plan_verification_suggestions(
            intent,
            current_query,
            state,
            recent_tool_history,
        ),
        acceptance_criteria: task_plan_acceptance_criteria(intent),
        status: if state.ready_to_finish {
            "ready"
        } else {
            "active"
        }
        .to_string(),
    };
    plan.open_questions.truncate(TASK_PLAN_MAX_OPEN_QUESTIONS);
    plan.suggested_tools
        .truncate(TASK_PLAN_MAX_TOOL_SUGGESTIONS);
    plan.suggested_agents
        .truncate(TASK_PLAN_MAX_AGENT_SUGGESTIONS);
    plan.verification_suggestions
        .truncate(TASK_PLAN_MAX_VERIFICATION_SUGGESTIONS);
    plan
}

pub(crate) fn render_task_plan_for_prompt(plan: &TaskPlan) -> Option<String> {
    if plan.goal.trim().is_empty() {
        return None;
    }
    let mut lines = vec![
        "## Task Plan".to_string(),
        "- Treat this as soft guidance; if evidence or blockers suggest a better route, adapt and explain the reason.".to_string(),
        format!("- Goal: {}", plan.goal),
        format!("- Intent: {}", plan.intent),
        format!("- Status: {}", plan.status),
    ];
    if !plan.steps.is_empty() {
        lines.push("- Steps:".to_string());
        for step in &plan.steps {
            lines.push(format!(
                "  - [{}] {} ({})",
                step.status, step.title, step.id
            ));
        }
    }
    if !plan.open_questions.is_empty() {
        lines.push("- Open questions:".to_string());
        for question in &plan.open_questions {
            lines.push(format!("  - {question}"));
        }
    }
    if !plan.suggested_tools.is_empty() {
        lines.push("- Suggested tools:".to_string());
        for tool in &plan.suggested_tools {
            lines.push(format!(
                "  - `{}`: {} (source {}, score {})",
                tool.name, tool.reason, tool.source, tool.score
            ));
        }
    }
    if !plan.suggested_agents.is_empty() {
        lines.push("- Suggested agents:".to_string());
        for agent in &plan.suggested_agents {
            lines.push(format!(
                "  - `{}`: {} (score {})",
                agent.name, agent.reason, agent.score
            ));
        }
    }
    if !plan.verification_suggestions.is_empty() {
        lines.push(
            "- Verification suggestions (do not run automatically; choose when useful):"
                .to_string(),
        );
        for item in &plan.verification_suggestions {
            lines.push(format!(
                "  - `{}` [{} {}]: {}",
                item.command, item.confidence, item.when, item.reason
            ));
        }
    }
    if !plan.acceptance_criteria.is_empty() {
        lines.push("- Acceptance criteria:".to_string());
        for item in &plan.acceptance_criteria {
            lines.push(format!("  - {item}"));
        }
    }
    let rendered = lines.join("\n");
    if rendered.len() <= TASK_PLAN_PROMPT_CHAR_BUDGET {
        return Some(rendered);
    }
    let marker = "\n*(task plan truncated)*";
    let keep = TASK_PLAN_PROMPT_CHAR_BUDGET.saturating_sub(marker.len());
    Some(format!("{}{}", crate::truncate(&rendered, keep), marker))
}

pub(crate) fn task_plan_tool_ranking_context(plan: &TaskPlan) -> crate::tools::ToolRankingContext {
    let mut ranking = crate::tools::ToolRankingContext::default();
    for tool in &plan.suggested_tools {
        ranking.add_preference(
            tool.name.clone(),
            tool.reason.clone(),
            tool.score,
            crate::tools::ToolRankingSource::from_label(&tool.source),
        );
    }
    for item in &plan.verification_suggestions {
        ranking.add_preference(
            "exec",
            format!("verification suggestion: {}", item.command),
            3,
            crate::tools::ToolRankingSource::Plan,
        );
    }
    ranking
}

fn task_intent_label(intent: TaskIntent) -> &'static str {
    match intent {
        TaskIntent::Inform => "inform",
        TaskIntent::Change => "change",
        TaskIntent::Investigate => "investigate",
        TaskIntent::Execute => "execute",
    }
}

fn task_plan_steps(state: &WorkingState) -> Vec<TaskPlanStep> {
    let intent = state.intent;
    let inspected = !state.evidence.is_empty() || !state.completed_steps.is_empty();
    let changed = state.has_successful_change_trace();
    let executed = state.has_successful_execution_trace();
    let verified = state.completed_steps.iter().any(|step| {
        step.to_ascii_lowercase().contains("test")
            || step.to_ascii_lowercase().contains("build")
            || step.to_ascii_lowercase().contains("check")
    });
    let mut steps = Vec::new();
    steps.push(TaskPlanStep {
        id: "inspect".to_string(),
        title: match intent {
            TaskIntent::Inform => "Gather enough context to answer accurately",
            TaskIntent::Change => "Inspect the affected code and constraints",
            TaskIntent::Investigate => "Reproduce or narrow the issue with focused evidence",
            TaskIntent::Execute => "Identify the command, target, and success signal",
        }
        .to_string(),
        status: if inspected { "completed" } else { "pending" }.to_string(),
    });
    if matches!(intent, TaskIntent::Change) {
        steps.push(TaskPlanStep {
            id: "change".to_string(),
            title: "Apply the smallest coherent code or config change".to_string(),
            status: if changed {
                "completed"
            } else if inspected {
                "pending"
            } else {
                "blocked"
            }
            .to_string(),
        });
    }
    if matches!(
        intent,
        TaskIntent::Execute | TaskIntent::Change | TaskIntent::Investigate
    ) {
        steps.push(TaskPlanStep {
            id: "verify".to_string(),
            title: "Validate the result with targeted checks".to_string(),
            status: if verified {
                "completed"
            } else if executed || changed || inspected {
                "pending"
            } else {
                "blocked"
            }
            .to_string(),
        });
    }
    steps.push(TaskPlanStep {
        id: "finish".to_string(),
        title: "Summarize outcome, evidence, and any remaining risk".to_string(),
        status: if state.ready_to_finish {
            "ready"
        } else {
            "pending"
        }
        .to_string(),
    });
    steps
}

fn task_plan_open_questions(state: &WorkingState) -> Vec<String> {
    state
        .open_questions
        .iter()
        .cloned()
        .chain(
            state
                .uncertainties
                .iter()
                .filter(|item| item.blocking)
                .map(|item| format!("{}: {}", item.topic, item.reason)),
        )
        .filter_map(|item| sanitize_state_text(&item, WORKING_STATE_MAX_TEXT_CHARS))
        .collect()
}

fn task_plan_tool_suggestions(
    intent: TaskIntent,
    current_query: Option<&str>,
    available_tools: &[String],
    recent_tool_history: &[ToolResultEntry],
) -> Vec<TaskPlanToolSuggestion> {
    let mut suggestions = Vec::new();
    let query = current_query.unwrap_or_default().to_ascii_lowercase();
    let mut push = |name: &str, reason: &str, score: usize, source: &str| {
        if !available_tools
            .iter()
            .any(|tool| tool.eq_ignore_ascii_case(name))
        {
            return;
        }
        if let Some(existing) = suggestions
            .iter_mut()
            .find(|item: &&mut TaskPlanToolSuggestion| item.name.eq_ignore_ascii_case(name))
        {
            if source == "recent_failure" {
                existing.reason = reason.to_string();
                existing.score = existing.score.max(score);
                existing.source = source.to_string();
            }
            return;
        }
        suggestions.push(TaskPlanToolSuggestion {
            name: name.to_string(),
            reason: reason.to_string(),
            score,
            source: source.to_string(),
        });
    };
    push(
        "think",
        "Keep the next action explicit before using tools",
        2,
        "intent",
    );
    match intent {
        TaskIntent::Inform => {
            push(
                "search_files",
                "Find relevant local context before answering",
                4,
                "intent",
            );
            push(
                "read_file",
                "Read the strongest matching source directly",
                4,
                "intent",
            );
            if contains_any(&query, &["http", "https", "docs", "官方", "latest", "最新"]) {
                push(
                    "http_fetch",
                    "Verify referenced external material",
                    3,
                    "query",
                );
            }
        }
        TaskIntent::Change => {
            push(
                "search_files",
                "Locate affected code and tests",
                5,
                "intent",
            );
            push(
                "read_file",
                "Inspect current implementation before editing",
                5,
                "intent",
            );
            push("patch_file", "Apply a scoped change", 4, "intent");
            push(
                "exec",
                "Run targeted verification when the change is ready",
                4,
                "intent",
            );
        }
        TaskIntent::Investigate => {
            push(
                "search_files",
                "Find likely owners and error paths",
                5,
                "intent",
            );
            push(
                "read_file",
                "Inspect evidence instead of guessing",
                5,
                "intent",
            );
            push(
                "exec",
                "Reproduce or validate the suspected failure",
                4,
                "intent",
            );
            if contains_any(&query, &["http", "https", "docs", "官方", "latest", "最新"]) {
                push(
                    "http_fetch",
                    "Check external docs or referenced URLs",
                    3,
                    "query",
                );
            }
        }
        TaskIntent::Execute => {
            push(
                "exec",
                "Execute the requested command or verification",
                5,
                "intent",
            );
            push(
                "read_file",
                "Read config or scripts when command context is unclear",
                3,
                "intent",
            );
        }
    }
    for result in recent_tool_history
        .iter()
        .rev()
        .filter(|result| result.is_error)
        .take(2)
    {
        match result.name.as_str() {
            "exec" => push(
                "read_file",
                "Recent exec failed; inspect config or scripts before retrying",
                4,
                "recent_failure",
            ),
            "read_file" => push(
                "search_files",
                "Recent read failed; search for the correct path",
                4,
                "recent_failure",
            ),
            _ => push(
                "think",
                "Recent tool failure needs a revised approach",
                3,
                "recent_failure",
            ),
        }
    }
    suggestions.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
    suggestions
}

fn task_plan_agent_suggestions(
    intent: TaskIntent,
    current_query: Option<&str>,
    available_agents: &[String],
) -> Vec<TaskPlanAgentSuggestion> {
    let query = current_query.unwrap_or_default().to_ascii_lowercase();
    let mut suggestions = Vec::new();
    for agent in available_agents {
        let lower = agent.to_ascii_lowercase();
        let score = match intent {
            TaskIntent::Inform if contains_any(&lower, &["research", "explore", "review"]) => 3,
            TaskIntent::Change if contains_any(&lower, &["coder", "backend", "frontend"]) => 4,
            TaskIntent::Investigate if contains_any(&lower, &["review", "explore", "research"]) => {
                4
            }
            TaskIntent::Execute if contains_any(&lower, &["coder", "general"]) => 2,
            _ => 0,
        } + usize::from(!query.is_empty() && query.contains(&lower));
        if score > 0 {
            suggestions.push(TaskPlanAgentSuggestion {
                name: agent.clone(),
                reason: "Agent specialization appears relevant to the task intent".to_string(),
                score,
            });
        }
    }
    suggestions.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
    suggestions
}

fn task_plan_verification_suggestions(
    intent: TaskIntent,
    current_query: Option<&str>,
    state: &WorkingState,
    recent_tool_history: &[ToolResultEntry],
) -> Vec<TaskPlanVerificationSuggestion> {
    if matches!(intent, TaskIntent::Inform) {
        return Vec::new();
    }
    let signals = task_plan_observed_signal_text(current_query, state, recent_tool_history);
    let lower = signals.to_ascii_lowercase();
    let mut items = Vec::new();
    if contains_any(
        &lower,
        &["src/", "src\\", "cargo", "rust", ".rs", "cargo.toml", "mcp"],
    ) {
        push_task_plan_verification_suggestion(
            &mut items,
            "cargo fmt --check",
            "Rust code or Cargo metadata appears relevant",
            "high",
            "before_finish",
        );
        if lower.contains("mcp") {
            push_task_plan_verification_suggestion(
                &mut items,
                "cargo test mcp",
                "MCP behavior appears relevant",
                "high",
                "before_finish",
            );
        } else {
            push_task_plan_verification_suggestion(
                &mut items,
                "cargo test",
                "Rust behavior appears relevant",
                "medium",
                "before_finish",
            );
        }
    }
    if contains_any(
        &lower,
        &["frontend/", "frontend\\", ".tsx", ".ts", "npm", "vitest"],
    ) {
        push_task_plan_verification_suggestion(
            &mut items,
            "npm run typecheck",
            "Frontend TypeScript appears relevant",
            "high",
            "before_finish",
        );
        push_task_plan_verification_suggestion(
            &mut items,
            "npm test",
            "Frontend behavior appears relevant",
            "medium",
            "before_finish",
        );
    }
    if contains_any(
        &lower,
        &["readme", "docs/", "docs\\", ".md", ".json", "config"],
    ) {
        push_task_plan_verification_suggestion(
            &mut items,
            "git diff --check",
            "Docs or config text changed or was inspected",
            "medium",
            "before_finish",
        );
    }
    if items.is_empty() && matches!(intent, TaskIntent::Change | TaskIntent::Execute) {
        push_task_plan_verification_suggestion(
            &mut items,
            "git diff --check",
            "Check for whitespace or patch formatting issues",
            "low",
            "before_finish",
        );
    }
    items
}

fn push_task_plan_verification_suggestion(
    items: &mut Vec<TaskPlanVerificationSuggestion>,
    command: &str,
    reason: &str,
    confidence: &str,
    when: &str,
) {
    if items.iter().any(|item| item.command == command) {
        return;
    }
    items.push(TaskPlanVerificationSuggestion {
        command: command.to_string(),
        reason: reason.to_string(),
        confidence: confidence.to_string(),
        when: when.to_string(),
    });
}

fn task_plan_observed_signal_text(
    current_query: Option<&str>,
    state: &WorkingState,
    recent_tool_history: &[ToolResultEntry],
) -> String {
    let mut parts = Vec::new();
    if let Some(query) = current_query {
        parts.push(query.to_string());
    }
    if let Some(goal) = state.primary_goal.as_ref() {
        parts.push(goal.clone());
    }
    parts.extend(state.completed_steps.iter().cloned());
    parts.extend(state.evidence.iter().map(|item| item.claim.clone()));
    parts.extend(state.open_questions.iter().cloned());
    parts.extend(
        state
            .uncertainties
            .iter()
            .map(|item| format!("{} {}", item.topic, item.reason)),
    );
    for result in recent_tool_history {
        parts.push(result.name.clone());
        if let Some(summary) = result.trace_summary() {
            parts.push(summary.to_string());
        }
        parts.push(crate::truncate(&result.result, 400));
    }
    parts.join(" ")
}

fn task_plan_acceptance_criteria(intent: TaskIntent) -> Vec<String> {
    match intent {
        TaskIntent::Inform => vec![
            "Answer directly with the evidence or assumptions used.".to_string(),
            "Call out uncertainty if the available context is incomplete.".to_string(),
        ],
        TaskIntent::Change => vec![
            "The change is scoped to the requested behavior.".to_string(),
            "Relevant verification commands are suggested or run before finishing.".to_string(),
            "The final response states files changed and residual risk.".to_string(),
        ],
        TaskIntent::Investigate => vec![
            "The final answer identifies likely cause or narrowed hypotheses.".to_string(),
            "Evidence and failed paths are clearly separated.".to_string(),
        ],
        TaskIntent::Execute => vec![
            "The requested command or workflow outcome is reported.".to_string(),
            "Errors include the next actionable recovery step.".to_string(),
        ],
    }
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn sanitize_state_text(text: &str, max_chars: usize) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() {
        return None;
    }
    let truncated: String = trimmed.chars().take(max_chars).collect();
    if truncated.trim().is_empty() {
        None
    } else {
        Some(truncated)
    }
}

fn query_is_follow_up_continuation(query: &str) -> bool {
    let normalized = query
        .trim()
        .trim_matches(|c: char| {
            c.is_whitespace()
                || c.is_ascii_punctuation()
                || matches!(c, '，' | '。' | '！' | '？' | '：' | '；' | '、')
        })
        .to_ascii_lowercase();
    [
        "continue",
        "go on",
        "keep going",
        "what next",
        "next step",
        "proceed",
        "继续",
        "接着",
        "下一步",
        "接下来",
        "然后呢",
    ]
    .iter()
    .any(|phrase| continuation_phrase_matches(&normalized, phrase))
}

fn continuation_phrase_matches(normalized: &str, phrase: &str) -> bool {
    normalized == phrase
        || normalized
            .strip_prefix(phrase)
            .is_some_and(continuation_tail_is_filler)
        || normalized
            .strip_suffix(phrase)
            .is_some_and(continuation_tail_is_filler)
}

fn continuation_tail_is_filler(tail: &str) -> bool {
    let tail = tail.trim().trim_matches(|c: char| {
        c.is_whitespace()
            || c.is_ascii_punctuation()
            || matches!(c, '，' | '。' | '！' | '？' | '：' | '；' | '、')
    });
    if tail.is_empty() {
        return true;
    }

    let ascii_tokens = tail
        .split_whitespace()
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if tail.is_ascii() && !ascii_tokens.is_empty() {
        return ascii_tokens.iter().all(|token| {
            matches!(
                *token,
                "please" | "pls" | "plz" | "now" | "again" | "thanks" | "thank" | "you"
            )
        });
    }

    matches!(
        tail,
        "一下"
            | "下"
            | "吧"
            | "呀"
            | "啊"
            | "呢"
            | "哦"
            | "哈"
            | "一下吧"
            | "一下呢"
            | "一下呀"
            | "一下哦"
            | "一下哈"
    )
}

fn query_represents_new_goal(previous: &str, next: &str) -> bool {
    if previous.eq_ignore_ascii_case(next) {
        return false;
    }

    let previous_lower = previous.to_ascii_lowercase();
    let next_lower = next.to_ascii_lowercase();
    if previous_lower.contains(&next_lower) || next_lower.contains(&previous_lower) {
        return false;
    }

    let previous_tokens = crate::tokenize_for_matching(&previous_lower)
        .into_iter()
        .filter(|token| !is_low_signal_query_token(token) && !is_goal_action_token(token))
        .collect::<Vec<_>>();
    let next_tokens = crate::tokenize_for_matching(&next_lower)
        .into_iter()
        .filter(|token| !is_low_signal_query_token(token) && !is_goal_action_token(token))
        .collect::<Vec<_>>();
    if previous_tokens.is_empty() || next_tokens.is_empty() {
        return true;
    }

    let shared = previous_tokens
        .iter()
        .filter(|token| next_tokens.contains(token))
        .count();
    let min_tokens = previous_tokens.len().min(next_tokens.len());
    let required_shared = if min_tokens >= 4 { 2 } else { 1 };
    shared < required_shared
}

fn is_low_signal_query_token(token: &str) -> bool {
    matches!(
        token,
        "the"
            | "this"
            | "that"
            | "these"
            | "those"
            | "with"
            | "from"
            | "into"
            | "onto"
            | "then"
            | "next"
            | "step"
            | "continue"
            | "继续"
            | "接着"
            | "下一步"
            | "接下来"
    )
}

fn is_goal_action_token(token: &str) -> bool {
    matches!(
        token,
        "implement"
            | "fix"
            | "modify"
            | "edit"
            | "update"
            | "refactor"
            | "optimize"
            | "rewrite"
            | "patch"
            | "run"
            | "execute"
            | "install"
            | "launch"
            | "start"
            | "stop"
            | "build"
            | "compile"
            | "deploy"
            | "benchmark"
            | "profile"
            | "diagnose"
            | "investigate"
            | "debug"
            | "review"
            | "inspect"
            | "analyze"
            | "trace"
            | "实现"
            | "修复"
            | "修改"
            | "更新"
            | "重构"
            | "优化"
            | "运行"
            | "执行"
            | "安装"
            | "启动"
            | "编译"
            | "诊断"
            | "排查"
            | "调查"
            | "分析"
            | "审查"
            | "检查"
    )
}

fn normalized_key(text: &str) -> String {
    text.to_ascii_lowercase()
}

fn push_unique_text(target: &mut Vec<String>, value: String, limit: usize, max_chars: usize) {
    let Some(value) = sanitize_state_text(&value, max_chars) else {
        return;
    };
    let key = normalized_key(&value);
    if target
        .iter()
        .any(|existing| normalized_key(existing) == key)
    {
        return;
    }
    target.push(value);
    trim_to_latest_items(target, limit);
}

fn push_unique_evidence(target: &mut Vec<EvidenceItem>, value: EvidenceItem, limit: usize) {
    let Some(claim) = sanitize_state_text(&value.claim, WORKING_STATE_MAX_TEXT_CHARS) else {
        return;
    };
    let source_tool =
        sanitize_state_text(&value.source_tool, 64).unwrap_or_else(|| "tool".to_string());
    let source_ref = sanitize_state_text(&value.source_ref, 96).unwrap_or_default();
    let dedupe_source_ref = normalized_evidence_source_ref_for_dedupe(&source_ref);
    let key = format!(
        "{}|{}|{}",
        normalized_key(&claim),
        normalized_key(&source_tool),
        dedupe_source_ref
    );
    if target.iter().any(|existing| {
        format!(
            "{}|{}|{}",
            normalized_key(&existing.claim),
            normalized_key(&existing.source_tool),
            normalized_evidence_source_ref_for_dedupe(&existing.source_ref)
        ) == key
    }) {
        return;
    }
    target.push(EvidenceItem {
        claim,
        source_tool,
        source_ref,
        confidence: value.confidence,
    });
    trim_to_latest_items(target, limit);
}

fn normalized_evidence_source_ref_for_dedupe(source_ref: &str) -> String {
    let normalized = normalized_key(source_ref.trim());
    if normalized.is_empty() {
        return normalized;
    }
    if source_ref_looks_ephemeral_for_dedupe(&normalized) {
        String::new()
    } else {
        normalized
    }
}

fn source_ref_looks_ephemeral_for_dedupe(source_ref: &str) -> bool {
    if source_ref.starts_with("tool_call_")
        || source_ref.starts_with("call_")
        || source_ref.starts_with("toolu_")
        || source_ref.starts_with("tool_")
        || source_ref.starts_with("fc_")
    {
        return true;
    }

    if uuid_like_source_ref(source_ref) {
        return true;
    }

    source_ref.len() >= 16
        && !source_ref
            .chars()
            .any(|c| matches!(c, '/' | '\\' | '.' | ':' | '#' | '?' | ' '))
        && source_ref
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

fn uuid_like_source_ref(source_ref: &str) -> bool {
    if source_ref.len() != 36 {
        return false;
    }

    for (idx, ch) in source_ref.chars().enumerate() {
        if matches!(idx, 8 | 13 | 18 | 23) {
            if ch != '-' {
                return false;
            }
        } else if !ch.is_ascii_hexdigit() {
            return false;
        }
    }

    true
}

fn push_unique_uncertainty(
    target: &mut Vec<UncertaintyItem>,
    value: UncertaintyItem,
    limit: usize,
) {
    let Some(topic) = sanitize_state_text(&value.topic, 96) else {
        return;
    };
    let Some(reason) = sanitize_state_text(&value.reason, WORKING_STATE_MAX_TEXT_CHARS) else {
        return;
    };
    let key = format!("{}|{}", normalized_key(&topic), normalized_key(&reason));
    if target.iter().any(|existing| {
        format!(
            "{}|{}",
            normalized_key(&existing.topic),
            normalized_key(&existing.reason)
        ) == key
    }) {
        return;
    }
    target.push(UncertaintyItem {
        topic,
        reason,
        blocking: value.blocking,
    });
    trim_to_latest_items(target, limit);
}

fn trim_to_latest_items<T>(target: &mut Vec<T>, limit: usize) {
    if target.len() > limit {
        let excess = target.len() - limit;
        // New observations are appended at the tail; when we exceed the cap,
        // drop the oldest entries from the front so the freshest state survives.
        target.drain(..excess);
    }
}

fn reconcile_results_with_task_memory(
    state: &mut WorkingState,
    results: &[ToolResultEntry],
    task_memory: &crate::memory::RetrievedTaskMemory,
) {
    let anchors = crate::memory::task_memory_resolution_anchors(task_memory);

    for result in results.iter().filter(|result| !result.is_error) {
        let matched = anchors
            .iter()
            .filter(|anchor| result_mentions_anchor(result, anchor))
            .cloned()
            .collect::<Vec<_>>();
        if matched.is_empty() {
            continue;
        }

        for anchor in matched.iter().take(RULE_BASED_EVIDENCE_PER_TOOL) {
            push_unique_evidence(
                &mut state.evidence,
                EvidenceItem {
                    claim: format!("Validated remembered anchor: {anchor}"),
                    source_tool: result.name.clone(),
                    source_ref: preferred_evidence_source_ref(result),
                    confidence: EvidenceConfidence::Medium,
                },
                WORKING_STATE_MAX_ITEMS,
            );
        }

        state
            .uncertainties
            .retain(|item| !text_mentions_any_anchor(&item.topic, &matched, Some(&item.reason)));
        state
            .open_questions
            .retain(|item| !text_mentions_any_anchor(item, &matched, None));
        state
            .next_actions
            .retain(|item| !text_mentions_any_anchor(item, &matched, None));
    }

    state.recompute_ready_to_finish();

    let should_seed_memory_actions = results.iter().any(|result| result.is_error)
        || (state.intent.is_action_oriented()
            && (!state.ready_to_finish || state.next_actions.is_empty()));
    if !should_seed_memory_actions {
        return;
    }

    for action in crate::memory::task_memory_next_actions(task_memory, state.intent)
        .into_iter()
        .take(2)
    {
        push_unique_text(
            &mut state.next_actions,
            action,
            WORKING_STATE_MAX_ITEMS,
            WORKING_STATE_MAX_TEXT_CHARS,
        );
    }
}

fn text_mentions_any_anchor(text: &str, anchors: &[String], extra: Option<&str>) -> bool {
    anchors.iter().any(|anchor| {
        text_mentions_anchor(text, anchor)
            || extra
                .map(|candidate| text_mentions_anchor(candidate, anchor))
                .unwrap_or(false)
    })
}

fn result_mentions_anchor(result: &ToolResultEntry, anchor: &str) -> bool {
    text_mentions_anchor(&result.result, anchor)
        || result
            .trace
            .as_ref()
            .map(|trace| {
                trace
                    .anchor_values()
                    .into_iter()
                    .any(|value| text_mentions_anchor(value, anchor))
            })
            .unwrap_or(false)
        || result
            .call_summary
            .as_deref()
            .map(|summary| text_mentions_anchor(summary, anchor))
            .unwrap_or(false)
}

fn text_mentions_anchor(text: &str, anchor: &str) -> bool {
    let text_lower = text.to_ascii_lowercase();
    let anchor_lower = anchor.to_ascii_lowercase();
    if text_lower.is_empty() || anchor_lower.is_empty() {
        return false;
    }

    if anchor_looks_like_command(&anchor_lower) {
        return command_anchor_matches_text(&text_lower, &anchor_lower);
    }

    if anchor_lower.len() >= 10 && text_lower.contains(&anchor_lower) {
        return true;
    }

    let anchor_tokens = crate::tokenize_for_matching(&anchor_lower);
    let text_tokens = crate::tokenize_for_matching(&text_lower);
    if anchor_tokens.is_empty() || text_tokens.is_empty() {
        return false;
    }

    if anchor_tokens.len() == 1 {
        let token = &anchor_tokens[0];
        return token.len() >= 4 && text_tokens.contains(token);
    }

    let shared = anchor_tokens
        .iter()
        .filter(|token| text_tokens.contains(token))
        .count();
    let threshold = if anchor_tokens.len() >= 4 { 3 } else { 2 };
    shared >= threshold
}

fn anchor_looks_like_command(anchor: &str) -> bool {
    anchor.split_whitespace().next().is_some_and(|token| {
        matches!(
            token,
            "cargo"
                | "cargo.exe"
                | "npm"
                | "pnpm"
                | "yarn"
                | "git"
                | "python"
                | "python3"
                | "py"
                | "uv"
                | "go"
                | "node"
                | "bash"
                | "sh"
                | "powershell"
                | "powershell.exe"
                | "pwsh"
                | "pwsh.exe"
                | "make"
                | "cmake"
                | "docker"
                | "kubectl"
                | "terraform"
                | "mvn"
                | "mvnw"
        )
    })
}

fn command_anchor_matches_text(text: &str, anchor: &str) -> bool {
    let normalized_text = normalize_anchor_match_text(text);
    let normalized_anchor = normalize_anchor_match_text(anchor);
    let Some(start) = normalized_text.find(&normalized_anchor) else {
        return false;
    };
    let end = start + normalized_anchor.len();
    anchor_match_boundary_ok(normalized_text[..start].chars().next_back())
        && anchor_match_boundary_ok(normalized_text[end..].chars().next())
}

fn normalize_anchor_match_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn anchor_match_boundary_ok(ch: Option<char>) -> bool {
    match ch {
        None => true,
        Some(ch) => {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ';' | '!'
                )
        }
    }
}

fn text_indicates_structured_action_progress(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    contains_any(
        &lower,
        &[
            "implemented",
            "fixed",
            "updated",
            "modified",
            "patched",
            "wrote",
            "created",
            "deleted",
            "executed",
            "ran ",
            "built",
            "compiled",
            "installed",
            "deployed",
            "restarted",
            "passed",
            "changes applied",
            "implemented the change",
            "修复",
            "修改",
            "更新",
            "应用",
            "执行",
            "运行",
            "写入",
            "创建",
            "删除",
            "通过",
        ],
    )
}

fn text_indicates_structured_change_progress(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    contains_any(
        &lower,
        &[
            "implemented",
            "fixed",
            "updated",
            "modified",
            "patched",
            "wrote",
            "created",
            "deleted",
            "changes applied",
            "implemented the change",
            "修复",
            "修改",
            "更新",
            "应用",
            "写入",
            "创建",
            "删除",
        ],
    )
}

fn text_indicates_structured_exec_progress(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    contains_any(
        &lower,
        &[
            "test result: ok",
            "tests passed",
            "build succeeded",
            "compiled successfully",
            "benchmark complete",
            "profile written",
            "installed successfully",
            "deployed successfully",
            "migration complete",
            "validated successfully",
            "通过测试",
            "构建成功",
            "编译成功",
            "安装成功",
            "部署成功",
        ],
    )
}

fn completed_step_counts_as_execution_progress(step: &str) -> bool {
    let lower = step.to_ascii_lowercase();
    if lower.starts_with("execution progress:") {
        return true;
    }
    if [
        "write_file succeeded",
        "patch_file succeeded",
        "delete_file succeeded",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }

    contains_any(
        &lower,
        &[
            "applied",
            "updated",
            "modified",
            "changed",
            "patched",
            "wrote",
            "created",
            "deleted",
            "executed",
            "ran ",
            "built",
            "compiled",
            "installed",
            "deployed",
            "restarted",
            "tested",
            "修改",
            "更新",
            "应用",
            "执行",
            "运行",
            "写入",
            "创建",
            "删除",
            "修复",
        ],
    )
}

fn completed_step_counts_as_change_progress(step: &str) -> bool {
    let lower = step.to_ascii_lowercase();
    if [
        "write_file succeeded",
        "patch_file succeeded",
        "delete_file succeeded",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }

    if !lower.starts_with("execution progress:") {
        return false;
    }

    contains_any(
        &lower,
        &[
            "via write_file",
            "via patch_file",
            "via delete_file",
            "write `",
            "patch `",
            "delete `",
            "cargo fix",
            "git apply",
            "terraform apply",
        ],
    ) || text_indicates_structured_change_progress(&lower)
}

fn record_tool_failure(state: &mut WorkingState, result: &ToolResultEntry) {
    let base_reason = summarize_result_snippets(&result.result, 1)
        .into_iter()
        .next()
        .unwrap_or_else(|| "tool call failed".to_string());
    let reason = result
        .trace_summary()
        .map(|summary| format!("{base_reason} while trying to {summary}"))
        .unwrap_or(base_reason);
    push_unique_uncertainty(
        &mut state.uncertainties,
        UncertaintyItem {
            topic: format!("{} failure", result.name),
            reason,
            blocking: true,
        },
        WORKING_STATE_MAX_ITEMS,
    );
    push_unique_text(
        &mut state.open_questions,
        format!("How should {} be retried or replaced?", result.name),
        WORKING_STATE_MAX_ITEMS,
        WORKING_STATE_MAX_TEXT_CHARS,
    );
    push_unique_text(
        &mut state.next_actions,
        suggested_next_action_for_tool(&result.name),
        WORKING_STATE_MAX_ITEMS,
        WORKING_STATE_MAX_TEXT_CHARS,
    );
}

fn completed_step_for_result(result: &ToolResultEntry) -> Option<String> {
    if result_counts_as_execution_progress(result) {
        if let Some(summary) = result.trace_summary() {
            return Some(format!(
                "execution progress: {} via {} in {}ms (call {}).",
                summary, result.name, result.duration_ms, result.id
            ));
        }
        return Some(format!(
            "execution progress: {} completed in {}ms (call {}).",
            result.name, result.duration_ms, result.id
        ));
    }
    if let Some(summary) = result.trace_summary() {
        return Some(format!(
            "{} succeeded: {} in {}ms (call {}).",
            result.name, summary, result.duration_ms, result.id
        ));
    }
    Some(format!(
        "{} succeeded in {}ms (call {}).",
        result.name, result.duration_ms, result.id
    ))
}

fn result_counts_as_execution_progress(result: &ToolResultEntry) -> bool {
    match result.name.as_str() {
        "write_file" | "patch_file" | "delete_file" => true,
        "exec" => exec_result_counts_as_execution_progress(result),
        "task" | "orchestrate" => text_indicates_structured_action_progress(&result.result),
        _ => false,
    }
}

fn result_counts_as_change_progress(result: &ToolResultEntry) -> bool {
    match result.name.as_str() {
        "write_file" | "patch_file" | "delete_file" => true,
        "exec" => exec_result_counts_as_change_progress(result),
        "task" | "orchestrate" => text_indicates_structured_change_progress(&result.result),
        _ => false,
    }
}

fn exec_result_counts_as_execution_progress(result: &ToolResultEntry) -> bool {
    result
        .trace
        .as_ref()
        .and_then(|trace| trace.command.as_deref())
        .or_else(|| {
            result
                .call_summary
                .as_deref()
                .and_then(command_from_exec_summary)
        })
        .map(command_counts_as_execution_progress)
        .unwrap_or_else(|| text_indicates_structured_exec_progress(&result.result))
}

fn exec_result_counts_as_change_progress(result: &ToolResultEntry) -> bool {
    result
        .trace
        .as_ref()
        .and_then(|trace| trace.command.as_deref())
        .or_else(|| {
            result
                .call_summary
                .as_deref()
                .and_then(command_from_exec_summary)
        })
        .map(command_counts_as_change_progress)
        .unwrap_or(false)
}

fn command_from_exec_summary(summary: &str) -> Option<&str> {
    let summary = summary.strip_prefix("run `")?;
    let (command, _) = summary.split_once('`')?;
    (!command.trim().is_empty()).then_some(command)
}

fn command_counts_as_execution_progress(command: &str) -> bool {
    let normalized = normalize_command_for_progress(command);
    if normalized.is_empty() {
        return false;
    }
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return false;
    }

    match tokens.as_slice() {
        [cmd, sub, ..] if is_cargo_command(cmd) => matches!(
            *sub,
            "test" | "build" | "check" | "bench" | "run" | "install" | "clippy" | "fix"
        ),
        [cmd, sub, ..] if is_node_package_manager(cmd) => match *sub {
            "test" | "install" | "ci" | "start" => true,
            "run" => tokens
                .get(2)
                .is_some_and(|nested| script_name_counts_as_execution_progress(nested)),
            _ => false,
        },
        ["python", "-m", module, ..] | ["python3", "-m", module, ..] | ["py", "-m", module, ..] => {
            *module == "pytest"
        }
        ["uv", "run", tool, ..] => matches!(*tool, "pytest" | "cargo" | "go"),
        ["pytest", ..] | ["tox", ..] => true,
        ["go", sub, ..] => matches!(*sub, "test" | "build" | "run"),
        ["make", sub, ..] => matches!(*sub, "test" | "build" | "install" | "check"),
        ["cmake", "--build", ..] => true,
        [cmd, goal, ..] if is_gradle_command(cmd) => matches!(
            *goal,
            "test" | "build" | "check" | "assemble" | "run" | "bootrun"
        ),
        ["mvn", goal, ..] | ["mvnw", goal, ..] => {
            matches!(*goal, "test" | "package" | "install" | "verify")
        }
        ["docker", "build", ..] => true,
        ["docker", "compose", sub, ..] => matches!(*sub, "build" | "up" | "down" | "restart"),
        ["git", sub, ..] => matches!(*sub, "apply" | "am" | "cherry-pick" | "merge" | "pull"),
        ["kubectl", sub, ..] => matches!(*sub, "apply" | "rollout" | "delete"),
        ["terraform", "apply", ..] => true,
        _ => false,
    }
}

fn command_counts_as_change_progress(command: &str) -> bool {
    let normalized = normalize_command_for_progress(command);
    if normalized.is_empty() {
        return false;
    }
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return false;
    }

    match tokens.as_slice() {
        [cmd, sub, ..] if is_cargo_command(cmd) => matches!(*sub, "fix"),
        ["git", sub, ..] => matches!(*sub, "apply" | "am" | "cherry-pick" | "merge" | "pull"),
        ["kubectl", sub, ..] => matches!(*sub, "apply" | "delete"),
        ["terraform", "apply", ..] => true,
        _ => false,
    }
}

fn normalize_command_for_progress(command: &str) -> String {
    let trimmed = command.trim();
    let mut lower = trimmed.to_ascii_lowercase();
    for prefix in [
        "cmd /c ",
        "cmd.exe /c ",
        "powershell -command ",
        "powershell.exe -command ",
        "pwsh -command ",
        "pwsh.exe -command ",
        "bash -c ",
        "bash -lc ",
        "sh -c ",
        "sh -lc ",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            lower = rest
                .trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string();
            break;
        }
    }
    lower
}

fn is_cargo_command(command: &str) -> bool {
    matches!(command, "cargo" | "cargo.exe")
}

fn is_node_package_manager(command: &str) -> bool {
    matches!(
        command,
        "npm" | "npm.cmd" | "pnpm" | "pnpm.cmd" | "yarn" | "yarn.cmd"
    )
}

fn is_gradle_command(command: &str) -> bool {
    matches!(
        command,
        "gradle" | "gradle.bat" | "./gradlew" | "gradlew" | ".\\gradlew.bat"
    )
}

fn script_name_counts_as_execution_progress(script: &str) -> bool {
    ["build", "test", "start", "bench", "benchmark"]
        .iter()
        .any(|family| script == *family || has_script_family_prefix(script, family))
}

fn has_script_family_prefix(script: &str, family: &str) -> bool {
    script
        .strip_prefix(family)
        .is_some_and(|suffix| matches!(suffix.chars().next(), Some(':' | '-' | '_')))
}

#[allow(dead_code)]
fn text_indicates_exec_progress(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    contains_any(
        &lower,
        &[
            "test result: ok",
            "tests passed",
            "build succeeded",
            "compiled successfully",
            "benchmark complete",
            "profile written",
            "installed successfully",
            "deployed successfully",
            "migration complete",
            "validated successfully",
            "通过测试",
            "构建成功",
            "编译成功",
            "安装成功",
            "部署成功",
        ],
    )
}

#[allow(dead_code)]
fn text_indicates_action_progress(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    contains_any(
        &lower,
        &[
            "implemented",
            "fixed",
            "updated",
            "modified",
            "patched",
            "wrote",
            "created",
            "deleted",
            "executed",
            "ran ",
            "built",
            "compiled",
            "installed",
            "deployed",
            "restarted",
            "passed",
            "changes applied",
            "implemented the change",
            "修复",
            "修改",
            "更新",
            "应用",
            "执行",
            "运行",
            "写入",
            "创建",
            "删除",
            "通过",
        ],
    )
}

fn evidence_from_result(result: &ToolResultEntry) -> Vec<EvidenceItem> {
    let label = match result.name.as_str() {
        "read_file" => "Observed file content",
        "search_files" => "Found code/reference match",
        "list_dir" => "Observed workspace entry",
        "http_fetch" => "Observed fetched content",
        _ => return Vec::new(),
    };
    let source_ref = preferred_evidence_source_ref(result);

    let snippets = summarize_result_snippets(&result.result, RULE_BASED_EVIDENCE_PER_TOOL);
    if snippets.is_empty() {
        return vec![EvidenceItem {
            claim: format!("{label} via {}.", result.name),
            source_tool: result.name.clone(),
            source_ref,
            confidence: EvidenceConfidence::High,
        }];
    }

    snippets
        .into_iter()
        .map(|snippet| EvidenceItem {
            claim: result
                .trace_summary()
                .map(|summary| format!("{label} ({summary}): {snippet}"))
                .unwrap_or_else(|| format!("{label}: {snippet}")),
            source_tool: result.name.clone(),
            source_ref: source_ref.clone(),
            confidence: EvidenceConfidence::High,
        })
        .collect()
}

fn preferred_evidence_source_ref(result: &ToolResultEntry) -> String {
    result
        .trace
        .as_ref()
        .and_then(ToolExecutionTrace::stable_source_ref)
        .and_then(|value| sanitize_state_text(value, 96))
        .unwrap_or_else(|| result.id.clone())
}

fn summarize_result_snippets(result: &str, limit: usize) -> Vec<String> {
    result
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| sanitize_state_text(line, WORKING_STATE_MAX_TEXT_CHARS))
        .take(limit)
        .collect()
}

fn suggested_next_action_for_tool(tool_name: &str) -> String {
    match tool_name {
        "read_file" => {
            "Retry read_file with a narrower path or inspect the parent directory first."
                .to_string()
        }
        "search_files" => {
            "Adjust the search pattern or narrow the search scope before retrying search_files."
                .to_string()
        }
        "list_dir" => {
            "Inspect the parent path or narrow the directory target before retrying list_dir."
                .to_string()
        }
        "http_fetch" => {
            "Check the URL or request parameters before retrying http_fetch.".to_string()
        }
        "exec" => {
            "Try a smaller command, different arguments, or inspect the relevant files first."
                .to_string()
        }
        _ => format!("Try a different approach or adjust the arguments for {tool_name}."),
    }
}

fn compact_evidence_ref(item: &EvidenceItem) -> String {
    if item.source_ref.is_empty() {
        item.source_tool.clone()
    } else {
        format!("{}:{}", item.source_tool, item.source_ref)
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolExecutionTrace {
    #[serde(default)]
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_glob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<usize>,
}

impl ToolExecutionTrace {
    pub(crate) fn summary(&self) -> Option<&str> {
        let trimmed = self.summary.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    fn stable_source_ref(&self) -> Option<&str> {
        self.path
            .as_deref()
            .or(self.secondary_path.as_deref())
            .or(self.url.as_deref())
            .or(self.working_dir.as_deref())
            .or(self.pattern.as_deref())
            .or(self.file_glob.as_deref())
            .or(self.command.as_deref())
            .or(self.agent.as_deref())
            .or(self.summary())
    }

    fn retry_key_ref(&self) -> Option<&str> {
        self.retry_key.as_deref()
    }

    fn fallback_retry_key(&self) -> Option<String> {
        let has_structured_args = self.command.is_some()
            || self.working_dir.is_some()
            || self.path.is_some()
            || self.secondary_path.is_some()
            || self.pattern.is_some()
            || self.file_glob.is_some()
            || self.url.is_some()
            || self.agent.is_some()
            || self.task_count.is_some()
            || self.start_line.is_some()
            || self.end_line.is_some()
            || self.max_results.is_some()
            || self.summary().is_some();
        if !has_structured_args {
            return None;
        }
        serde_json::to_string(self).ok()
    }

    fn anchor_values(&self) -> Vec<&str> {
        let mut values = Vec::new();
        if let Some(value) = self.command.as_deref() {
            values.push(value);
        }
        if let Some(value) = self.working_dir.as_deref() {
            values.push(value);
        }
        if let Some(value) = self.path.as_deref() {
            values.push(value);
        }
        if let Some(value) = self.secondary_path.as_deref() {
            values.push(value);
        }
        if let Some(value) = self.pattern.as_deref() {
            values.push(value);
        }
        if let Some(value) = self.file_glob.as_deref() {
            values.push(value);
        }
        if let Some(value) = self.url.as_deref() {
            values.push(value);
        }
        if let Some(value) = self.agent.as_deref() {
            values.push(value);
        }
        if let Some(value) = self.summary() {
            values.push(value);
        }
        values
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ToolResultEntry {
    pub id: String,
    pub name: String,
    pub result: String,
    pub duration_ms: u64,
    pub is_error: bool,
    pub call_summary: Option<String>,
    pub trace: Option<ToolExecutionTrace>,
}

impl ToolResultEntry {
    fn trace_summary(&self) -> Option<&str> {
        self.trace
            .as_ref()
            .and_then(ToolExecutionTrace::summary)
            .or(self.call_summary.as_deref())
    }
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
                    "{}{} [{status}, {}ms] returned {line_count} lines / {byte_size} bytes{}",
                    r.name,
                    r.trace_summary()
                        .map(|summary| format!(" ({summary})"))
                        .unwrap_or_default(),
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
#[cfg(test)]
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AutoObservationStrength {
    #[default]
    None,
    Light,
    Medium,
    Strong,
}

impl AutoObservationStrength {
    fn pressure(self) -> usize {
        match self {
            Self::None => 0,
            Self::Light => 1,
            Self::Medium => 2,
            Self::Strong => 3,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Light => "light",
            Self::Medium => "medium",
            Self::Strong => "strong",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AutoThinkLevel {
    #[default]
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl AutoThinkLevel {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    fn score(self) -> i32 {
        match self {
            Self::Low => 0,
            Self::Medium => 1,
            Self::High => 2,
            Self::Xhigh => 3,
            Self::Max => 4,
        }
    }

    fn from_score(score: i32) -> Self {
        match score.clamp(0, 4) {
            0 => Self::Low,
            1 => Self::Medium,
            2 => Self::High,
            3 => Self::Xhigh,
            _ => Self::Max,
        }
    }

    fn at_least(self, minimum: Self) -> Self {
        if self.score() < minimum.score() {
            minimum
        } else {
            self
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AutoRetryPattern {
    #[default]
    None,
    SameTool,
    SameArgs,
}

impl AutoRetryPattern {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SameTool => "same_tool",
            Self::SameArgs => "same_args",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AutoErrorKind {
    #[default]
    None,
    Timeout,
    Permission,
    Validation,
    MissingInput,
    Environment,
    Unknown,
}

impl AutoErrorKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Timeout => "timeout",
            Self::Permission => "permission",
            Self::Validation => "validation",
            Self::MissingInput => "missing_input",
            Self::Environment => "environment",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AutoEvidenceDeltaQuality {
    #[default]
    None,
    MoreEvidence,
    BetterEvidence,
    ResolvedBlocker,
    NoMeaningfulProgress,
}

impl AutoEvidenceDeltaQuality {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::MoreEvidence => "more_evidence",
            Self::BetterEvidence => "better_evidence",
            Self::ResolvedBlocker => "resolved_blocker",
            Self::NoMeaningfulProgress => "no_meaningful_progress",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AutoThinkTraceSignals {
    #[serde(default)]
    pub(crate) intent: String,
    #[serde(default)]
    pub(crate) user_msg_chars: usize,
    #[serde(default)]
    pub(crate) observation_strength: String,
    #[serde(default)]
    pub(crate) tool_results_count: usize,
    #[serde(default)]
    pub(crate) tool_error_count: usize,
    #[serde(default)]
    pub(crate) summary_count: usize,
    #[serde(default)]
    pub(crate) summary_bytes: usize,
    #[serde(default)]
    pub(crate) stagnation_streak: usize,
    #[serde(default)]
    pub(crate) error_streak: usize,
    #[serde(default)]
    pub(crate) task_pressure: usize,
    #[serde(default)]
    pub(crate) ready_to_finish: bool,
    #[serde(default)]
    pub(crate) action_oriented: bool,
    #[serde(default)]
    pub(crate) has_blocking_uncertainty: bool,
    #[serde(default)]
    pub(crate) progress_made: bool,
    #[serde(default)]
    pub(crate) retry_pattern: String,
    #[serde(default)]
    pub(crate) error_kind: String,
    #[serde(default)]
    pub(crate) evidence_delta_quality: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AutoThinkDecision {
    pub(crate) selected_level: AutoThinkLevel,
    pub(crate) baseline_level: AutoThinkLevel,
    pub(crate) baseline_reason: String,
    pub(crate) escalators: Vec<String>,
    pub(crate) dampeners: Vec<String>,
    pub(crate) clamps: Vec<String>,
    pub(crate) signals: AutoThinkTraceSignals,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AutoThinkTrace {
    #[serde(default)]
    pub(crate) round: usize,
    #[serde(default)]
    pub(crate) cycle: usize,
    #[serde(default)]
    pub(crate) phase: String,
    #[serde(default)]
    pub(crate) model: String,
    #[serde(default)]
    pub(crate) provider: String,
    #[serde(default)]
    pub(crate) selected_think: String,
    #[serde(default)]
    pub(crate) baseline_level: String,
    #[serde(default)]
    pub(crate) baseline_reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) escalators: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) dampeners: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) clamps: Vec<String>,
    #[serde(default)]
    pub(crate) signals: AutoThinkTraceSignals,
}

impl AutoThinkDecision {
    pub(crate) fn into_trace(
        self,
        round: usize,
        cycle: usize,
        phase: &str,
        model: &str,
        provider: &str,
    ) -> AutoThinkTrace {
        AutoThinkTrace {
            round,
            cycle,
            phase: phase.to_string(),
            model: model.to_string(),
            provider: provider.to_string(),
            selected_think: self.selected_level.label().to_string(),
            baseline_level: self.baseline_level.label().to_string(),
            baseline_reason: self.baseline_reason,
            escalators: self.escalators,
            dampeners: self.dampeners,
            clamps: self.clamps,
            signals: self.signals,
        }
    }

    pub(crate) fn into_trace_with_selected_think(
        self,
        round: usize,
        cycle: usize,
        phase: &str,
        model: &str,
        provider: &str,
        selected_think: &str,
    ) -> AutoThinkTrace {
        let mut trace = self.into_trace(round, cycle, phase, model, provider);
        if trace.selected_think != selected_think {
            trace.selected_think = selected_think.to_string();
            if !trace
                .clamps
                .iter()
                .any(|item| item == "hook_think_override")
            {
                trace.clamps.push("hook_think_override".to_string());
            }
        }
        trace
    }
}

impl AutoThinkTrace {
    pub(crate) fn to_live_event(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "auto_trace",
            "round": self.round,
            "cycle": self.cycle,
            "phase": self.phase,
            "model": self.model,
            "provider": self.provider,
            "selected_think": self.selected_think,
            "baseline_level": self.baseline_level,
            "baseline_reason": self.baseline_reason,
            "escalators": self.escalators,
            "dampeners": self.dampeners,
            "clamps": self.clamps,
            "signals": self.signals,
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AutoThinkRuntimeSignals {
    pub(crate) intent: TaskIntent,
    pub(crate) cycles: usize,
    pub(crate) observation_strength: AutoObservationStrength,
    pub(crate) user_msg_chars: usize,
    pub(crate) tool_results_count: usize,
    pub(crate) tool_error_count: usize,
    pub(crate) summary_count: usize,
    pub(crate) summary_bytes: usize,
    pub(crate) stagnation_streak: usize,
    pub(crate) error_streak: usize,
    pub(crate) task_pressure: usize,
    pub(crate) ready_to_finish: bool,
    pub(crate) action_oriented: bool,
    pub(crate) has_blocking_uncertainty: bool,
    pub(crate) progress_made: bool,
    pub(crate) retry_pattern: AutoRetryPattern,
    pub(crate) error_kind: AutoErrorKind,
    pub(crate) evidence_delta_quality: AutoEvidenceDeltaQuality,
}

impl AutoThinkRuntimeSignals {
    fn to_trace_signals(self) -> AutoThinkTraceSignals {
        AutoThinkTraceSignals {
            intent: self.intent.label().to_string(),
            user_msg_chars: self.user_msg_chars,
            observation_strength: self.observation_strength.label().to_string(),
            tool_results_count: self.tool_results_count,
            tool_error_count: self.tool_error_count,
            summary_count: self.summary_count,
            summary_bytes: self.summary_bytes,
            stagnation_streak: self.stagnation_streak,
            error_streak: self.error_streak,
            task_pressure: self.task_pressure,
            ready_to_finish: self.ready_to_finish,
            action_oriented: self.action_oriented,
            has_blocking_uncertainty: self.has_blocking_uncertainty,
            progress_made: self.progress_made,
            retry_pattern: self.retry_pattern.label().to_string(),
            error_kind: self.error_kind.label().to_string(),
            evidence_delta_quality: self.evidence_delta_quality.label().to_string(),
        }
    }
}

pub(crate) fn auto_observation_strength(
    results: &[ToolResultEntry],
    summaries: &[ObservationSummary],
) -> AutoObservationStrength {
    if results.is_empty() {
        return AutoObservationStrength::None;
    }

    let error_count = results.iter().filter(|result| result.is_error).count();
    let summary_count = summaries.len();
    let total_bytes = summaries
        .iter()
        .map(|summary| summary.byte_size)
        .sum::<usize>();

    if error_count >= 2
        || (error_count >= 1 && summary_count >= 1)
        || summary_count >= 3
        || total_bytes >= 24_000
    {
        AutoObservationStrength::Strong
    } else if error_count >= 1 || summary_count >= 1 || results.len() >= 2 {
        AutoObservationStrength::Medium
    } else {
        AutoObservationStrength::Light
    }
}

pub(crate) fn auto_think_progress_made(before: &WorkingState, after: &WorkingState) -> bool {
    after.completed_steps.len() > before.completed_steps.len()
        || after.evidence.len() > before.evidence.len()
        || after.open_questions.len() < before.open_questions.len()
        || after.uncertainties.len() < before.uncertainties.len()
        || after.next_actions.len() < before.next_actions.len()
        || (!before.ready_to_finish && after.ready_to_finish)
        || (!before.has_successful_execution_trace() && after.has_successful_execution_trace())
        || (!before.has_successful_change_trace() && after.has_successful_change_trace())
}

fn confirmed_evidence_count(state: &WorkingState) -> usize {
    state
        .evidence
        .iter()
        .filter(|item| item.confidence.is_confirmed())
        .count()
}

pub(crate) fn auto_evidence_delta_quality(
    before: &WorkingState,
    after: &WorkingState,
    progress_made: bool,
) -> AutoEvidenceDeltaQuality {
    if before.has_blocking_uncertainty() && !after.has_blocking_uncertainty() {
        return AutoEvidenceDeltaQuality::ResolvedBlocker;
    }
    if confirmed_evidence_count(after) > confirmed_evidence_count(before) {
        return AutoEvidenceDeltaQuality::BetterEvidence;
    }
    if after.evidence.len() > before.evidence.len() {
        return AutoEvidenceDeltaQuality::MoreEvidence;
    }
    if !progress_made {
        return AutoEvidenceDeltaQuality::NoMeaningfulProgress;
    }
    AutoEvidenceDeltaQuality::None
}

fn tool_result_retry_key(result: &ToolResultEntry) -> Option<String> {
    result
        .trace
        .as_ref()
        .and_then(|trace| {
            trace
                .retry_key_ref()
                .map(str::to_string)
                .or_else(|| trace.fallback_retry_key())
        })
        .or_else(|| result.call_summary.clone())
        .or_else(|| result.trace_summary().map(str::to_string))
}

pub(crate) fn auto_retry_pattern(history: &[ToolResultEntry]) -> AutoRetryPattern {
    let Some(last) = history.last() else {
        return AutoRetryPattern::None;
    };

    let same_tool_streak = history
        .iter()
        .rev()
        .take_while(|result| result.name == last.name)
        .count();
    if same_tool_streak < 2 {
        return AutoRetryPattern::None;
    }

    let Some(last_key) = tool_result_retry_key(last) else {
        return AutoRetryPattern::SameTool;
    };
    let same_args_streak = history
        .iter()
        .rev()
        .take_while(|result| {
            result.name == last.name
                && tool_result_retry_key(result).as_deref() == Some(last_key.as_str())
        })
        .count();
    if same_args_streak >= 2 {
        AutoRetryPattern::SameArgs
    } else {
        AutoRetryPattern::SameTool
    }
}

pub(crate) fn auto_error_kind(results: &[ToolResultEntry]) -> AutoErrorKind {
    let Some(last_error) = results.iter().rev().find(|result| result.is_error) else {
        return AutoErrorKind::None;
    };

    let lower = last_error.result.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        return AutoErrorKind::Timeout;
    }
    if lower.contains("permission denied")
        || lower.contains("access is denied")
        || lower.contains("not permitted")
        || lower.contains("forbidden")
        || lower.contains("unauthorized")
    {
        return AutoErrorKind::Permission;
    }
    if lower.contains("missing required parameter")
        || lower.contains("missing or invalid")
        || lower.contains("missing required")
    {
        return AutoErrorKind::MissingInput;
    }
    if lower.contains("invalid arguments")
        || lower.contains("arguments must")
        || lower.contains("cannot be null")
        || lower.contains("invalid parameter")
        || lower.contains("parameter '")
    {
        return AutoErrorKind::Validation;
    }
    if lower.contains("command not found")
        || lower.contains("failed to spawn")
        || lower.contains("connection refused")
        || lower.contains("not installed")
        || lower.contains("no such file")
        || lower.contains("not found")
        || lower.contains("unreachable")
    {
        return AutoErrorKind::Environment;
    }
    AutoErrorKind::Unknown
}

pub(crate) fn auto_think_task_pressure(state: &WorkingState) -> usize {
    let mut pressure = 0;
    if state.intent.is_action_oriented() && !state.ready_to_finish {
        pressure += 1;
    }
    if state.has_blocking_uncertainty() {
        pressure += 2;
    }
    if !state.open_questions.is_empty() {
        pressure += 1;
    }
    if state.next_actions.len() > 1 {
        pressure += 1;
    }
    if state.has_confirmed_evidence() && !state.ready_to_finish {
        pressure += 1;
    }
    pressure
}

fn auto_baseline_runtime(signals: AutoThinkRuntimeSignals) -> (AutoThinkLevel, &'static str) {
    match signals.cycles {
        0 => match signals.intent {
            TaskIntent::Inform => (AutoThinkLevel::Medium, "initial_inform"),
            TaskIntent::Investigate => (AutoThinkLevel::Medium, "initial_investigate"),
            TaskIntent::Change => (AutoThinkLevel::High, "initial_change"),
            TaskIntent::Execute => (AutoThinkLevel::High, "initial_execute"),
        },
        1..=5 => match signals.intent {
            TaskIntent::Inform => (AutoThinkLevel::Medium, "mid_loop_inform"),
            TaskIntent::Investigate => (AutoThinkLevel::Medium, "mid_loop_investigate"),
            TaskIntent::Change => (AutoThinkLevel::High, "mid_loop_change"),
            TaskIntent::Execute => (AutoThinkLevel::High, "mid_loop_execute"),
        },
        6..=10 => match signals.intent {
            TaskIntent::Inform => (AutoThinkLevel::Low, "late_loop_inform"),
            TaskIntent::Investigate => (AutoThinkLevel::Medium, "late_loop_investigate"),
            TaskIntent::Change => (AutoThinkLevel::Medium, "late_loop_change"),
            TaskIntent::Execute => (AutoThinkLevel::Medium, "late_loop_execute"),
        },
        _ => match signals.intent {
            TaskIntent::Inform => (AutoThinkLevel::Low, "deep_loop_inform"),
            TaskIntent::Investigate => (AutoThinkLevel::Low, "deep_loop_investigate"),
            TaskIntent::Change => (AutoThinkLevel::Medium, "deep_loop_change"),
            TaskIntent::Execute => (AutoThinkLevel::Medium, "deep_loop_execute"),
        },
    }
}

fn push_auto_reason(reasons: &mut Vec<String>, reason: &str) {
    if !reasons.iter().any(|item| item == reason) {
        reasons.push(reason.to_string());
    }
}

pub(crate) fn auto_think_decision_runtime(signals: AutoThinkRuntimeSignals) -> AutoThinkDecision {
    let (baseline_level, baseline_reason) = auto_baseline_runtime(signals);
    let mut score = baseline_level.score();
    let mut escalators = Vec::new();
    let mut dampeners = Vec::new();
    let mut clamps = Vec::new();
    let mut minimum_level = baseline_level;

    if signals.error_streak >= 6 || (signals.error_streak >= 4 && signals.stagnation_streak >= 2) {
        minimum_level = AutoThinkLevel::Max;
        push_auto_reason(&mut clamps, "severe_error_loop");
    } else {
        if signals.error_streak >= 4 {
            minimum_level = minimum_level.at_least(AutoThinkLevel::Xhigh);
            push_auto_reason(&mut clamps, "error_streak_xhigh");
        }
        if signals.stagnation_streak >= 5 {
            minimum_level = minimum_level.at_least(AutoThinkLevel::Xhigh);
            push_auto_reason(&mut clamps, "severe_stagnation");
        }
    }

    if signals.cycles == 0 {
        if signals.user_msg_chars > 600 {
            minimum_level = minimum_level.at_least(AutoThinkLevel::Xhigh);
            push_auto_reason(&mut clamps, "large_initial_request");
        } else if signals.user_msg_chars > 220 {
            minimum_level = minimum_level.at_least(AutoThinkLevel::High);
            push_auto_reason(&mut clamps, "complex_initial_request");
        }
    }

    if signals.error_streak >= 2 {
        score += 1;
        push_auto_reason(&mut escalators, "error_streak");
    }
    if signals.stagnation_streak >= 3 {
        score += 1;
        push_auto_reason(&mut escalators, "stagnation_streak");
    }
    if signals.has_blocking_uncertainty {
        score += 1;
        push_auto_reason(&mut escalators, "blocking_uncertainty");
    }
    if signals.task_pressure >= 4 {
        score += 2;
        push_auto_reason(&mut escalators, "task_pressure_high");
    } else if signals.task_pressure >= 2 {
        score += 1;
        push_auto_reason(&mut escalators, "task_pressure");
    }

    match signals.observation_strength {
        AutoObservationStrength::Strong => {
            score += 1;
            push_auto_reason(&mut escalators, "strong_observation");
        }
        AutoObservationStrength::Medium if signals.action_oriented || signals.cycles <= 5 => {
            score += 1;
            push_auto_reason(&mut escalators, "medium_observation");
        }
        _ => {}
    }

    if !signals.progress_made {
        match signals.retry_pattern {
            AutoRetryPattern::SameArgs => {
                score += 2;
                push_auto_reason(&mut escalators, "retry_same_args");
            }
            AutoRetryPattern::SameTool => {
                score += 1;
                push_auto_reason(&mut escalators, "retry_same_tool");
            }
            AutoRetryPattern::None => {}
        }
    }

    match signals.error_kind {
        AutoErrorKind::Timeout => {
            score += 1;
            push_auto_reason(&mut escalators, "timeout_errors");
        }
        AutoErrorKind::Permission => {
            score += 1;
            push_auto_reason(&mut escalators, "permission_errors");
        }
        AutoErrorKind::Environment => {
            score += 1;
            push_auto_reason(&mut escalators, "environment_errors");
        }
        AutoErrorKind::Validation | AutoErrorKind::MissingInput
            if signals.error_streak >= 1
                || matches!(signals.retry_pattern, AutoRetryPattern::SameArgs) =>
        {
            score += 1;
            push_auto_reason(&mut escalators, "input_shape_errors");
        }
        _ => {}
    }

    if matches!(
        signals.evidence_delta_quality,
        AutoEvidenceDeltaQuality::NoMeaningfulProgress
    ) && signals.cycles > 0
    {
        score += 1;
        push_auto_reason(&mut escalators, "no_meaningful_progress");
    }

    if signals.ready_to_finish {
        score -= 1;
        push_auto_reason(&mut dampeners, "ready_to_finish");
    }
    if signals.progress_made {
        score -= 1;
        push_auto_reason(&mut dampeners, "recent_progress");
    }
    if matches!(
        signals.evidence_delta_quality,
        AutoEvidenceDeltaQuality::ResolvedBlocker
    ) {
        score -= 1;
        push_auto_reason(&mut dampeners, "resolved_blocker");
    }
    if matches!(
        signals.evidence_delta_quality,
        AutoEvidenceDeltaQuality::BetterEvidence
    ) && signals.ready_to_finish
    {
        score -= 1;
        push_auto_reason(&mut dampeners, "better_evidence");
    }
    if !signals.action_oriented && signals.task_pressure == 0 && signals.cycles >= 4 {
        score -= 1;
        push_auto_reason(&mut dampeners, "informational_low_pressure");
    }
    let converging_for_decay = signals.ready_to_finish
        || signals.progress_made
        || matches!(
            signals.evidence_delta_quality,
            AutoEvidenceDeltaQuality::BetterEvidence | AutoEvidenceDeltaQuality::ResolvedBlocker
        );
    if signals.cycles >= 6
        && converging_for_decay
        && !signals.has_blocking_uncertainty
        && signals.stagnation_streak == 0
        && signals.error_streak == 0
        && signals.task_pressure <= 1
        && signals.observation_strength.pressure() <= 1
        && matches!(signals.retry_pattern, AutoRetryPattern::None)
    {
        score -= 1;
        push_auto_reason(&mut dampeners, "late_loop_decay");
    }

    let mut selected_level = AutoThinkLevel::from_score(score).at_least(minimum_level);
    if signals.cycles >= 8
        && signals.ready_to_finish
        && signals.progress_made
        && !signals.has_blocking_uncertainty
        && signals.task_pressure <= 1
    {
        let converging_cap = if signals.action_oriented {
            AutoThinkLevel::Medium
        } else {
            AutoThinkLevel::Low
        };
        if selected_level.score() > converging_cap.score() {
            selected_level = converging_cap;
            push_auto_reason(&mut clamps, "converging_late_loop_cap");
        }
    }

    AutoThinkDecision {
        selected_level,
        baseline_level,
        baseline_reason: baseline_reason.to_string(),
        escalators,
        dampeners,
        clamps,
        signals: signals.to_trace_signals(),
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
    if query.contains('\n') {
        return false;
    }
    let lower = query.to_ascii_lowercase();
    if [
        "```", "`", "{", "}", "=>", "::", ".rs", ".py", ".ts", ".tsx",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return false;
    }
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
        "review",
        "optimize",
        "performance",
        "latency",
        "memory",
        "context",
        "strategy",
        "plan",
        "design",
        "architect",
        "diagnose",
        "investigate",
        "benchmark",
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
