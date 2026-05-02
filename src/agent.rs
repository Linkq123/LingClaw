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
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

static FINISH_GATE_DEFERRALS: AtomicU64 = AtomicU64::new(0);
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
    pub(crate) fn seed_from_query(&mut self, query: Option<&str>) {
        let Some(query) =
            query.and_then(|query| sanitize_state_text(query, WORKING_STATE_MAX_TEXT_CHARS))
        else {
            return;
        };
        if self.last_seeded_query.as_deref() == Some(query.as_str()) {
            return;
        }
        if self.last_seeded_query.is_some() && query_is_follow_up_continuation(&query) {
            return;
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

pub(crate) enum FinishDecision {
    ContinueToAct,
    Finish(FinishReason),
    Defer(String),
}

pub(crate) struct FinishGateContext<'a> {
    pub finish_deferred_once: bool,
    pub assistant_content: Option<&'a str>,
    pub memory_next_actions: &'a [String],
    pub working_state: &'a WorkingState,
}

pub(crate) fn evaluate_finish_with_gate(
    has_content: bool,
    has_tool_calls: bool,
    context: &FinishGateContext<'_>,
) -> FinishDecision {
    let Some(reason) = evaluate_finish(has_content, has_tool_calls) else {
        return FinishDecision::ContinueToAct;
    };
    if reason == FinishReason::Empty || context.finish_deferred_once {
        return FinishDecision::Finish(reason);
    }

    if should_defer_finish(context) {
        FINISH_GATE_DEFERRALS.fetch_add(1, Ordering::Relaxed);
        return FinishDecision::Defer(build_finish_gate_hint(context));
    }

    FinishDecision::Finish(reason)
}

pub(crate) fn finish_gate_metrics() -> u64 {
    FINISH_GATE_DEFERRALS.load(Ordering::Relaxed)
}

fn should_defer_finish(context: &FinishGateContext<'_>) -> bool {
    let state = context.working_state;

    if state.has_blocking_uncertainty() {
        return true;
    }

    if state.intent.is_action_oriented() && !state.ready_to_finish {
        return true;
    }

    state.has_confirmed_evidence()
        && !answer_is_grounded_in_state(state, context.assistant_content.unwrap_or(""))
}

fn build_finish_gate_hint(context: &FinishGateContext<'_>) -> String {
    let state = context.working_state;
    let mut lines = vec![
        "## Finish Check".to_string(),
        "Before finishing, verify that the user's task is genuinely complete.".to_string(),
    ];

    if state.has_blocking_uncertainty() {
        lines.push(
            "There is still at least one blocking uncertainty. Resolve it or explain the remaining gap explicitly.".to_string(),
        );
    }

    if state.intent.is_action_oriented() && !state.ready_to_finish {
        lines.push(
            "Action-oriented work still needs at least one confirmed finding or successful execution trace before you wrap up.".to_string(),
        );
    }

    if state.has_confirmed_evidence()
        && !answer_is_grounded_in_state(state, context.assistant_content.unwrap_or(""))
    {
        lines.push(
            "You already gathered evidence. Use the concrete findings in your answer instead of giving a generic wrap-up.".to_string(),
        );
    }

    if let Some(blocker) = prioritized_blocking_uncertainty(state) {
        lines.push(format!("Top blocker: {}", format_uncertainty_hint(blocker)));
    }
    if state.has_confirmed_evidence()
        && let Some(evidence) = prioritized_confirmed_evidence(state)
    {
        lines.push(format!(
            "Strongest evidence: [{}] {} ({})",
            evidence.confidence.label(),
            evidence.claim,
            compact_evidence_ref(evidence)
        ));
    }

    let next_steps = prioritized_finish_hint_actions(state, context.memory_next_actions);
    if !next_steps.is_empty() {
        lines.push("Suggested next actions:".to_string());
        for action in next_steps {
            lines.push(format!("- {action}"));
        }
    }
    if !context.memory_next_actions.is_empty() {
        lines.push("Relevant memory follow-ups:".to_string());
        for action in context
            .memory_next_actions
            .iter()
            .take(TASK_STATE_MAX_NEXT_ACTIONS)
        {
            lines.push(format!("- {action}"));
        }
    }

    lines.join("\n")
}

fn prioritized_blocking_uncertainty(state: &WorkingState) -> Option<&UncertaintyItem> {
    state.uncertainties.iter().rev().find(|item| item.blocking)
}

fn prioritized_confirmed_evidence(state: &WorkingState) -> Option<&EvidenceItem> {
    state
        .evidence
        .iter()
        .rev()
        .find(|item| matches!(item.confidence, EvidenceConfidence::High))
        .or_else(|| {
            state
                .evidence
                .iter()
                .rev()
                .find(|item| item.confidence.is_confirmed())
        })
}

fn prioritized_finish_hint_actions<'a>(
    state: &'a WorkingState,
    memory_next_actions: &'a [String],
) -> Vec<&'a str> {
    let mut actions = Vec::new();
    let mut seen = HashSet::new();

    for action in state
        .next_actions
        .iter()
        .chain(memory_next_actions.iter())
        .filter_map(|action| {
            let trimmed = action.trim();
            (!trimmed.is_empty()).then_some(trimmed)
        })
    {
        let key = normalized_key(action);
        if seen.insert(key) {
            actions.push(action);
            if actions.len() >= TASK_STATE_MAX_NEXT_ACTIONS {
                break;
            }
        }
    }

    actions
}

fn format_uncertainty_hint(item: &UncertaintyItem) -> String {
    let topic = item.topic.trim();
    let reason = item.reason.trim();
    match (topic.is_empty(), reason.is_empty()) {
        (false, false) => format!("{topic} - {reason}"),
        (false, true) => topic.to_string(),
        (true, false) => reason.to_string(),
        (true, true) => "blocking uncertainty remains unresolved".to_string(),
    }
}

fn answer_is_grounded_in_state(state: &WorkingState, content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    if !state.has_confirmed_evidence() {
        return true;
    }

    let answer_tokens = crate::tokenize_for_matching(trimmed)
        .into_iter()
        .filter(|token| token.len() >= 2)
        .collect::<HashSet<_>>();
    if answer_tokens.is_empty() {
        return false;
    }

    state
        .evidence
        .iter()
        .filter(|item| item.confidence.is_confirmed())
        .any(|item| {
            if source_ref_supports_grounding_match(&item.source_ref)
                && text_mentions_anchor(trimmed, &item.source_ref)
            {
                return true;
            }
            let matched_tokens = crate::tokenize_for_matching(&item.claim)
                .into_iter()
                .filter(|token| token.len() >= 2)
                .filter(|token| answer_tokens.contains(token))
                .collect::<HashSet<_>>();
            matched_tokens.len() >= 2
                || matched_tokens.iter().any(|token| token.len() >= 6)
                || (answer_tokens.len() == 1
                    && matched_tokens.len() == 1
                    && matched_tokens
                        .iter()
                        .all(|token| token_looks_like_exact_value(token)))
        })
}

fn source_ref_supports_grounding_match(source_ref: &str) -> bool {
    source_ref.chars().count() > 3
}

fn token_looks_like_exact_value(token: &str) -> bool {
    if token.is_empty() || !token.chars().any(|ch| ch.is_ascii_digit()) {
        return false;
    }
    if token.chars().all(|ch| ch.is_ascii_digit()) {
        return true;
    }
    if let Some(version) = token.strip_prefix('v') {
        return !version.is_empty() && version.chars().all(|ch| ch.is_ascii_digit() || ch == '.');
    }

    let digit_prefix_len = token
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(idx, _)| idx)
        .unwrap_or(token.len());
    if digit_prefix_len == 0 || digit_prefix_len == token.len() {
        return false;
    }

    let suffix = &token[digit_prefix_len..];
    !suffix.is_empty()
        && suffix.chars().count() <= 4
        && suffix
            .chars()
            .all(|ch| ch.is_ascii_alphabetic() || matches!(ch, '%' | 'x'))
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
    if consecutive_errors >= 4 {
        return "xhigh";
    }
    // Consecutive tool failures: escalate to deeper thinking
    if consecutive_errors >= 2 {
        return "high";
    }

    // Very large first-turn requests usually need a deeper initial pass.
    if cycles == 0 {
        if user_msg_chars > 600 {
            return "xhigh";
        }
        if user_msg_chars > 220 {
            return "high";
        }
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
