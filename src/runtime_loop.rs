use super::*;

use crate::prompts::{SystemPromptToolMode, build_system_prompt_with_query_cached_for_tool_mode};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64};
use tokio::time::MissedTickBehavior;

mod socket_input;

pub(crate) use socket_input::{
    IdleSocketInputAction, ensure_session_ready, handle_idle_socket_input,
    resolve_or_create_socket_session, resolve_session_target_for_command,
    resolve_session_target_for_delete,
};
use socket_input::{drain_busy_socket_messages, persist_pending_interventions};

/// Minimum reasoning cycles before a reflection is worthwhile.
const REFLECTION_MIN_CYCLES: usize = 3;

/// Minimum cooldown between consecutive reflections (seconds).
const REFLECTION_COOLDOWN_SECS: i64 = 600; // 10 minutes

/// Epoch-seconds timestamp of the last reflection run (0 = never).
static LAST_REFLECTION_EPOCH: AtomicI64 = AtomicI64::new(0);

/// Runtime switch for whether post-execution reflection is currently enabled.
static REFLECTION_RUNTIME_ENABLED: AtomicBool = AtomicBool::new(false);

/// Generation counter for reflection runtime policy updates.
static REFLECTION_RUNTIME_GENERATION: AtomicU64 = AtomicU64::new(1);

/// Active background reflection cancellations keyed by an internal task id.
static ACTIVE_REFLECTION_CANCELS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<u64, CancellationToken>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

/// Monotonic ids for background reflection tasks.
static NEXT_REFLECTION_TASK_ID: AtomicU64 = AtomicU64::new(1);

/// Monotonic counter used to make fallback task ids unique even if the system
/// clock has coarse granularity or multiple tasks start within the same tick.
static NEXT_FALLBACK_TASK_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
pub(crate) fn reflection_test_guard() -> &'static tokio::sync::Mutex<()> {
    static REFLECTION_TEST_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));
    &REFLECTION_TEST_LOCK
}

fn epoch_secs_now() -> i64 {
    chrono::Local::now().timestamp()
}

/// Decide whether the current run warrants a post-execution reflection
/// **and** atomically claim the cooldown slot if so.
///
/// Returns `Some((previous_epoch, claimed_epoch))` when the caller wins the
/// slot.  Pass both values to `rollback_reflection_claim()` on failure/no-op.
/// Returns `None` when the cooldown hasn't elapsed or cycles are too few.
fn try_claim_reflection(cycles: usize, _tool_calls: usize) -> Option<(i64, i64)> {
    if cycles < REFLECTION_MIN_CYCLES {
        return None;
    }
    let now = epoch_secs_now();
    let last = LAST_REFLECTION_EPOCH.load(std::sync::atomic::Ordering::Relaxed);
    if now - last < REFLECTION_COOLDOWN_SECS {
        return None;
    }
    // Atomically swap in `now`; if another thread already swapped, the CAS
    // fails and we back off — only one reflection per cooldown window.
    LAST_REFLECTION_EPOCH
        .compare_exchange(
            last,
            now,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Relaxed,
        )
        .ok()
        .map(|prev| (prev, now))
}

/// Roll back a previously claimed cooldown slot so the next non-trivial run
/// can trigger a reflection (used when the reflection was a no-op or failed).
///
/// Uses CAS to restore the previous epoch only if no other run has claimed a
/// newer slot in the meantime — safe even when reflection timeout exceeds the
/// cooldown duration.
fn rollback_reflection_claim(previous: i64, claimed: i64) {
    let _ = LAST_REFLECTION_EPOCH.compare_exchange(
        claimed,
        previous,
        std::sync::atomic::Ordering::AcqRel,
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Return runtime reflection status for the `/reflection` command.
pub(crate) fn reflection_runtime_status() -> String {
    let last = LAST_REFLECTION_EPOCH.load(std::sync::atomic::Ordering::Relaxed);
    if last == 0 {
        return "Last reflection: never (since server start)".to_string();
    }
    let now = epoch_secs_now();
    let elapsed = now - last;
    let remaining = REFLECTION_COOLDOWN_SECS - elapsed;
    if remaining > 0 {
        format!(
            "Last reflection: {}s ago (cooldown: {}s remaining)",
            elapsed, remaining
        )
    } else {
        format!(
            "Last reflection: {}s ago (cooldown elapsed, ready)",
            elapsed
        )
    }
}

pub(crate) fn refresh_reflection_runtime(enabled: bool) -> u64 {
    let previous = REFLECTION_RUNTIME_ENABLED.swap(enabled, std::sync::atomic::Ordering::AcqRel);
    if previous == enabled {
        return REFLECTION_RUNTIME_GENERATION.load(std::sync::atomic::Ordering::Acquire);
    }
    REFLECTION_RUNTIME_GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1
}

pub(crate) fn reflection_runtime_enabled() -> bool {
    REFLECTION_RUNTIME_ENABLED.load(std::sync::atomic::Ordering::Acquire)
}

fn reflection_runtime_generation() -> u64 {
    REFLECTION_RUNTIME_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

fn reflection_runtime_matches(generation: u64) -> bool {
    REFLECTION_RUNTIME_ENABLED.load(std::sync::atomic::Ordering::Acquire)
        && REFLECTION_RUNTIME_GENERATION.load(std::sync::atomic::Ordering::Acquire) == generation
}

fn register_active_reflection(cancel: CancellationToken) -> u64 {
    let task_id = NEXT_REFLECTION_TASK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    match ACTIVE_REFLECTION_CANCELS.lock() {
        Ok(mut guard) => {
            guard.insert(task_id, cancel);
        }
        Err(poisoned) => {
            eprintln!("Warning: reflection cancel registry poisoned during register; recovering");
            let mut guard = poisoned.into_inner();
            guard.insert(task_id, cancel);
        }
    }
    task_id
}

fn finish_active_reflection(task_id: u64) {
    match ACTIVE_REFLECTION_CANCELS.lock() {
        Ok(mut guard) => {
            guard.remove(&task_id);
        }
        Err(poisoned) => {
            eprintln!("Warning: reflection cancel registry poisoned during cleanup; recovering");
            let mut guard = poisoned.into_inner();
            guard.remove(&task_id);
        }
    }
}

pub(crate) fn cancel_active_reflections() {
    let cancels = match ACTIVE_REFLECTION_CANCELS.lock() {
        Ok(mut guard) => guard.drain().map(|(_, cancel)| cancel).collect::<Vec<_>>(),
        Err(poisoned) => {
            eprintln!("Warning: reflection cancel registry poisoned during cancel; recovering");
            let mut guard = poisoned.into_inner();
            guard.drain().map(|(_, cancel)| cancel).collect::<Vec<_>>()
        }
    };

    for cancel in cancels {
        cancel.cancel();
    }
}

pub(crate) struct AgentRunOutcome {
    pub(crate) rerun_agent: bool,
    pub(crate) shutting_down: bool,
    pub(crate) run_stopped: bool,
    pub(crate) run_failed: bool,
}

pub(crate) struct AgentRunReservation {
    connection_id: u64,
    run_cancel: CancellationToken,
    deferred_interventions: Arc<Mutex<DeferredInterventionState>>,
}

pub(crate) async fn try_reserve_agent_run(
    state: &Arc<AppState>,
    session_id: &str,
    connection_id: u64,
    cancel: &CancellationToken,
    stop_requested: &Arc<AtomicBool>,
) -> Option<AgentRunReservation> {
    let run_cancel = cancel.child_token();
    let deferred_interventions = Arc::new(Mutex::new(DeferredInterventionState::open()));
    let mut runs = state.active_runs.lock().await;
    if runs.contains_key(session_id) {
        return None;
    }
    runs.insert(
        session_id.to_string(),
        SessionRunBinding {
            connection_id,
            cancel: run_cancel.clone(),
            stop_requested: stop_requested.clone(),
            deferred_interventions: deferred_interventions.clone(),
        },
    );
    Some(AgentRunReservation {
        connection_id,
        run_cancel,
        deferred_interventions,
    })
}

pub(crate) async fn release_agent_run_reservation(
    state: &Arc<AppState>,
    session_id: &str,
    reservation: &AgentRunReservation,
) {
    let mut runs = state.active_runs.lock().await;
    if runs.get(session_id).map(|run| run.connection_id) == Some(reservation.connection_id) {
        runs.remove(session_id);
    }
}

pub(crate) async fn release_agent_run_for_stop_requested(
    state: &AppState,
    session_id: &str,
    stop_requested: &Arc<AtomicBool>,
) -> bool {
    let mut runs = state.active_runs.lock().await;
    let should_remove = runs
        .get(session_id)
        .is_some_and(|run| Arc::ptr_eq(&run.stop_requested, stop_requested));
    let removed = should_remove.then(|| runs.remove(session_id)).flatten();
    drop(runs);
    if let Some(run) = removed {
        run.cancel.cancel();
        return true;
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentRunMode {
    Execute,
    PlanOnly,
}

impl AgentRunMode {
    fn is_plan_only(self) -> bool {
        matches!(self, Self::PlanOnly)
    }
}

struct AgentRunCtx<'a> {
    state: &'a Arc<AppState>,
    current_session_id: &'a str,
    cancel: &'a CancellationToken,
    live_tx: &'a LiveTx,
    run_cancel: &'a CancellationToken,
}

struct AgentPhaseState {
    round: usize,
    pending_tool_calls: Vec<ToolCall>,
    collected_results: Vec<agent::ToolResultEntry>,
    results_origin_query: Option<String>,
    working_state: agent::WorkingState,
    run_mode: AgentRunMode,
    task_plan: Option<agent::TaskPlan>,
    retrieved_task_memory: Option<memory::RetrievedTaskMemory>,
    retrieved_task_memory_key: Option<String>,
    retrieved_task_memory_cycle: Option<usize>,
    cycle_workspace: PathBuf,
    last_observation_hint: Option<String>,
    last_observation_strength: agent::AutoObservationStrength,
    last_tool_results_count: usize,
    last_tool_error_count: usize,
    last_summary_count: usize,
    last_summary_bytes: usize,
    last_progress_made: bool,
    last_error_kind: agent::AutoErrorKind,
    last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality,
    stagnation_streak: usize,
    error_streak: usize,
    recent_tool_history: Vec<agent::ToolResultEntry>,
    pending_interventions: Vec<String>,
    react_ctx: agent::AgentLoopCtx,
    shutting_down: bool,
    run_stopped: bool,
    run_failed: bool,
    run_detached: bool,
    last_save_instant: Option<std::time::Instant>,
    /// Token counters snapshotted at loop start for per-round delta calculation.
    usage_snap_input: u64,
    usage_snap_output: u64,
}

/// Minimum interval between observe-phase incremental saves.
const OBSERVE_SAVE_DEBOUNCE: Duration = Duration::from_secs(5);
const AUTO_TOOL_HISTORY_CAP: usize = 12;
const DYNAMIC_PROMPT_OPTIONAL_SECTIONS_CHAR_BUDGET: usize = 4_000;
const DYNAMIC_PROMPT_TRUNCATION_MARKER: &str = "\n*(additional dynamic context truncated)*";
const PLAN_ONLY_PROMPT_SECTION: &str = "## Plan Mode\n\
You are in plan-only mode. Explore with read-only tools when helpful, then stop with a concrete implementation plan.\n\
- Do not modify files, execute shell commands, update todos, delegate to agents, or claim work has been performed.\n\
- The final answer must be a plan with goal, key implementation steps, affected areas, verification suggestions, and risks or open questions.\n\
- Wait for the user to approve execution before making changes.";

#[derive(Debug)]
enum AgentPhaseControl {
    Continue,
    Break,
}

struct AnalyzeSnapshot {
    model: String,
    usage_role: &'static str,
    think_level: String,
    pruned_count: usize,
    /// Character count of latest user message, for complexity-aware think level.
    user_msg_chars: usize,
    latest_query: Option<String>,
}

struct WorkingStateSessionData {
    latest_query: Option<String>,
    workspace: PathBuf,
    fallback_model: String,
}

enum ToolRunState {
    Completed(tools::ToolOutcome),
    Abort,
}

#[derive(Clone, Debug)]
enum WorkingStateDigestIssue {
    ProviderError(String),
    Timeout,
    InvalidJson(String),
}

/// Drop guard that sends a best-effort `task_failed` event when a `task` tool
/// future is dropped after `task_started` was emitted but before the terminal
/// event fired (e.g. on timeout or cancellation).
struct TaskEventGuard<'a> {
    live_tx: &'a LiveTx,
    agent_name: String,
    task_id: String,
    finished: bool,
}

impl<'a> TaskEventGuard<'a> {
    fn new(live_tx: &'a LiveTx, agent_name: &str, task_id: &str) -> Self {
        Self {
            live_tx,
            agent_name: agent_name.to_string(),
            task_id: task_id.to_string(),
            finished: false,
        }
    }

    fn mark_finished(&mut self) {
        self.finished = true;
    }
}

impl Drop for TaskEventGuard<'_> {
    fn drop(&mut self) {
        if !self.finished {
            eprintln!(
                "[task-guard] sub-agent '{}' dropped before terminal event — sending task_failed",
                self.agent_name
            );
            let event = json!({
                "type": "task_failed",
                "task_id": self.task_id,
                "agent": self.agent_name,
                "error": "task aborted (timeout or cancellation)",
            });
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let live_tx = self.live_tx.clone();
                handle.spawn(async move {
                    if let Err(err) = live_tx.send(event).await {
                        eprintln!("[task-guard] failed to emit fallback task_failed event: {err}");
                    }
                });
            } else if let Err(err) = self.live_tx.try_send(event) {
                eprintln!("[task-guard] failed to emit fallback task_failed event: {err}");
            }
        }
    }
}

const AGENT_HARD_CAP_ROUNDS: usize = 200;

struct PostExecutionReflectionInput {
    config: std::sync::Arc<Config>,
    http: reqwest::Client,
    sessions: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, Session>>>,
    session_id: String,
    workspace: std::path::PathBuf,
    model: String,
    messages: Vec<ChatMessage>,
    policy_generation: u64,
    cycles: usize,
    tool_calls: usize,
}

/// Post-execution reflection: analyze what went well/poorly in a multi-step task.
/// Writes a brief reflection to the session's daily memory file.
/// Runs as a non-blocking background task — failures are non-critical.
/// Returns `Ok(true)` when a reflection was actually written to disk,
/// `Ok(false)` when the conversation was too trivial for a meaningful reflection.
async fn run_post_execution_reflection(
    input: PostExecutionReflectionInput,
) -> Result<bool, String> {
    let PostExecutionReflectionInput {
        config,
        http,
        sessions,
        session_id,
        workspace,
        model,
        messages,
        policy_generation,
        cycles,
        tool_calls,
    } = input;

    if !reflection_runtime_matches(policy_generation) {
        return Ok(false);
    }

    // Build a compact excerpt of the conversation for reflection.
    let excerpt = crate::memory::build_conversation_excerpt(&messages);
    if excerpt.trim().is_empty() {
        return Ok(false);
    }
    // Cap excerpt to avoid excessive token use for reflection.
    let excerpt = crate::truncate(&excerpt, 8_000);

    let system_prompt = format!(
        "You are reflecting on a completed task. The task took {cycles} reasoning cycles \
         and {tool_calls} tool calls.\n\n\
         Analyze the conversation and produce 1-3 concise bullet points covering:\n\
         - What went well (efficient approaches, good decisions)\n\
         - What could be improved (wasted cycles, wrong tools, missed approaches)\n\
         - Key takeaway for future similar tasks\n\n\
         Be specific and actionable. Keep the same language as the conversation. \
         Return ONLY the bullet points, no preamble."
    );

    let prompt_messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some(system_prompt),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(excerpt),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let resolved = config.resolve_model(&model);
    let reflection = providers::call_llm_simple_with_usage(
        &http,
        &resolved,
        &prompt_messages,
        &workspace,
        config.s3.as_ref(),
        "off",
        config.max_llm_retries,
    )
    .await
    .map_err(|e| format!("Reflection LLM call failed: {e}"))?;

    let provider_name = config.resolve_provider_name(&model);
    let input_tokens = reflection.input_tokens.unwrap_or_else(|| {
        crate::estimate_tokens_for_provider(resolved.provider, &prompt_messages) as u64
    });
    let output_tokens = reflection.output_tokens.unwrap_or_else(|| {
        crate::message_token_len_for_provider(
            resolved.provider,
            &ChatMessage {
                role: "assistant".into(),
                content: Some(reflection.content.clone()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
        ) as u64
    });

    {
        let mut sessions = sessions.lock().await;
        if let Some(session) = sessions.get_mut(&session_id) {
            crate::update_session_token_usage_with_provider(
                session,
                input_tokens,
                output_tokens,
                token_usage_source(reflection.input_tokens),
                token_usage_source(reflection.output_tokens),
                Some(&provider_name),
                Some(crate::context::USAGE_ROLE_REFLECTION),
            );
        }
    }

    let reflection = reflection.content.trim().to_string();
    if reflection.is_empty() {
        return Ok(false);
    }

    if !reflection_runtime_matches(policy_generation) {
        return Ok(false);
    }

    // Write reflection to daily memory file.
    let local = prompts::current_local_snapshot();
    let today = local.today();
    let time = local.hhmm();
    let memory_dir = workspace.join("memory");
    let _ = tokio::fs::create_dir_all(&memory_dir).await;
    let memory_path = memory_dir.join(format!("{today}.md"));

    let entry = format!(
        "\n\n---\n\n## {time} Local — Reflection ({cycles} cycles, {tool_calls} tools)\n\n{reflection}"
    );
    let initial_content = format!("# {today}\n{entry}");

    use tokio::io::AsyncWriteExt;
    match tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&memory_path)
        .await
    {
        Ok(mut file) => {
            file.write_all(initial_content.as_bytes())
                .await
                .map_err(|e| format!("Write reflection: {e}"))?;
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut file = tokio::fs::OpenOptions::new()
                .append(true)
                .open(&memory_path)
                .await
                .map_err(|e| format!("Open memory file: {e}"))?;
            file.write_all(entry.as_bytes())
                .await
                .map_err(|e| format!("Write reflection: {e}"))?;
        }
        Err(e) => return Err(format!("Open memory file: {e}")),
    }

    Ok(true)
}

async fn send_react_phase_event(live_tx: &LiveTx, react_ctx: &agent::AgentLoopCtx, phase: &str) {
    if react_ctx.show_react {
        let _ = live_send(
            live_tx,
            json!({"type":"react_phase","phase":phase,"cycle":react_ctx.cycles}),
        )
        .await;
    }
}

fn select_analyze_model(
    config: &Config,
    base_model: &str,
    fast_model: Option<&str>,
    cycles: usize,
    has_model_override: bool,
    latest_query: Option<&str>,
    context_has_images: bool,
) -> (String, &'static str) {
    if cycles != 0 || has_model_override {
        return (base_model.to_string(), crate::context::USAGE_ROLE_PRIMARY);
    }

    let Some(fast_model) = fast_model else {
        return (base_model.to_string(), crate::context::USAGE_ROLE_PRIMARY);
    };

    let simple_query = latest_query.map(agent::is_simple_query).unwrap_or(false);
    let fast_supports_images = !context_has_images || config.model_supports_image(fast_model);
    if simple_query && fast_supports_images {
        (fast_model.to_string(), crate::context::USAGE_ROLE_FAST)
    } else {
        (base_model.to_string(), crate::context::USAGE_ROLE_PRIMARY)
    }
}

fn messages_have_images(messages: &[ChatMessage]) -> bool {
    messages.iter().any(|message| {
        message
            .images
            .as_ref()
            .is_some_and(|images| !images.is_empty())
    })
}

fn latest_user_query_from_messages(messages: &[ChatMessage]) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "user")
        .and_then(|message| message.content.clone())
}

fn refresh_retrieved_task_memory(
    phase_state: &mut AgentPhaseState,
    workspace: &Path,
    structured_memory_enabled: bool,
    current_query: Option<&str>,
    prefer_same_cycle_cache: bool,
) {
    if !structured_memory_enabled {
        phase_state.retrieved_task_memory = None;
        phase_state.retrieved_task_memory_key = None;
        phase_state.retrieved_task_memory_cycle = None;
        return;
    }

    let same_cycle = phase_state.retrieved_task_memory_cycle == Some(phase_state.react_ctx.cycles);
    if prefer_same_cycle_cache && same_cycle && phase_state.retrieved_task_memory.is_some() {
        return;
    }

    let cache_key = memory::task_memory_cache_key(current_query, Some(&phase_state.working_state));
    let can_reuse =
        same_cycle && phase_state.retrieved_task_memory_key.as_deref() == Some(cache_key.as_str());
    if can_reuse {
        return;
    }

    let retrieved = memory::retrieve_task_memory(
        &memory::load_structured_memory(workspace),
        current_query,
        Some(&phase_state.working_state),
    );
    phase_state.retrieved_task_memory = (!retrieved.is_empty()).then_some(retrieved);
    phase_state.retrieved_task_memory_key = Some(cache_key);
    phase_state.retrieved_task_memory_cycle = Some(phase_state.react_ctx.cycles);
}

fn append_dynamic_prompt_section(
    content: &mut String,
    remaining_budget: &mut usize,
    section: &str,
) -> bool {
    let section = section.trim();
    if section.is_empty() || *remaining_budget == 0 {
        return false;
    }

    const SECTION_SEPARATOR: &str = "\n\n";
    if *remaining_budget <= SECTION_SEPARATOR.len() {
        return false;
    }

    let available = *remaining_budget - SECTION_SEPARATOR.len();
    if section.len() <= available {
        content.push_str(SECTION_SEPARATOR);
        content.push_str(section);
        *remaining_budget -= SECTION_SEPARATOR.len() + section.len();
        return true;
    }

    let keep = available.saturating_sub(DYNAMIC_PROMPT_TRUNCATION_MARKER.len());
    if keep == 0 {
        return false;
    }
    content.push_str(SECTION_SEPARATOR);
    content.push_str(&crate::truncate(section, keep));
    content.push_str(DYNAMIC_PROMPT_TRUNCATION_MARKER);
    *remaining_budget = 0;
    true
}

fn append_owned_dynamic_prompt_section(
    content: &mut String,
    remaining_budget: &mut usize,
    slot: &mut Option<String>,
) {
    let should_clear = slot
        .as_deref()
        .is_some_and(|section| append_dynamic_prompt_section(content, remaining_budget, section));
    if should_clear {
        *slot = None;
    }
}

fn append_required_dynamic_prompt_section(content: &mut String, section: &str) -> bool {
    let section = section.trim();
    if section.is_empty() {
        return false;
    }

    content.push_str("\n\n");
    content.push_str(section);
    true
}

async fn prepare_analyze_snapshot(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
) -> Option<AnalyzeSnapshot> {
    let config = ctx.state.config();
    let mut sessions = ctx.state.sessions.lock().await;
    let session = sessions.get_mut(ctx.current_session_id)?;
    let base_model = session.effective_model(&config.model).to_string();
    let enabled_system_skills = session.enabled_system_skills.clone();

    // Extract latest user message for query-aware memory retrieval and complexity sensing.
    let latest_query = latest_user_query_from_messages(&session.messages);
    let redirected_to_new_goal = phase_state
        .working_state
        .seed_from_query(latest_query.as_deref());
    if redirected_to_new_goal {
        reset_runtime_auto_state_for_new_goal(phase_state);
    }
    let context_has_images = messages_have_images(&session.messages);
    let user_msg_chars = latest_query
        .as_ref()
        .map(|q| q.chars().count())
        .unwrap_or(0);

    let (model_str, usage_role) = select_analyze_model(
        &config,
        &base_model,
        config.fast_model.as_deref(),
        phase_state.react_ctx.cycles,
        session.model_override.is_some(),
        latest_query.as_deref(),
        context_has_images,
    );

    let system_prompt_tool_mode = if phase_state.run_mode.is_plan_only() {
        SystemPromptToolMode::PlanOnly
    } else {
        SystemPromptToolMode::Execute
    };
    let mut fresh_system = build_system_prompt_with_query_cached_for_tool_mode(
        &config,
        &session.workspace,
        &model_str,
        &enabled_system_skills,
        latest_query.as_deref(),
        system_prompt_tool_mode,
    );

    refresh_retrieved_task_memory(
        phase_state,
        &session.workspace,
        config.structured_memory,
        latest_query.as_deref(),
        false,
    );
    let discovered_agents = crate::subagents::discovery::discover_all_agents(&session.workspace);
    let task_plan = if config.enable_task_plan {
        let available_tools = if phase_state.run_mode.is_plan_only() {
            available_tool_names_for_plan_only(&config, &session.workspace)
        } else {
            available_tool_names_for_plan(&config, &session.workspace)
        };
        let available_agent_names = if phase_state.run_mode.is_plan_only() {
            Vec::new()
        } else {
            discovered_agents
                .iter()
                .map(|agent| agent.name.clone())
                .collect::<Vec<_>>()
        };
        Some(agent::build_task_plan(
            &phase_state.working_state,
            latest_query.as_deref(),
            &available_tools,
            &available_agent_names,
            &phase_state.recent_tool_history,
        ))
    } else {
        None
    };
    phase_state.task_plan = task_plan.clone();

    // Dynamic context injections into the system prompt:
    // - Required session state that must survive optional-context truncation
    // - Observation hint from previous cycle
    // - Planning nudge on first cycle for multi-step tasks
    if let Some(ref mut content) = fresh_system.content {
        let todos_section = crate::todos::render_prompt_section(&session.todos);
        let _ = append_required_dynamic_prompt_section(content, &todos_section);
        if phase_state.run_mode.is_plan_only() {
            let _ = append_required_dynamic_prompt_section(content, PLAN_ONLY_PROMPT_SECTION);
        }

        let mut remaining_budget = DYNAMIC_PROMPT_OPTIONAL_SECTIONS_CHAR_BUDGET;
        append_owned_dynamic_prompt_section(
            content,
            &mut remaining_budget,
            &mut phase_state.last_observation_hint,
        );
        if let Some(task_state) = agent::render_task_state_for_prompt(&phase_state.working_state) {
            let _ = append_dynamic_prompt_section(content, &mut remaining_budget, &task_state);
        }
        if let Some(task_plan) = task_plan.as_ref()
            && let Some(task_plan_prompt) = agent::render_task_plan_for_prompt(task_plan)
        {
            let _ =
                append_dynamic_prompt_section(content, &mut remaining_budget, &task_plan_prompt);
        }
        if let Some(task_memory) =
            phase_state
                .retrieved_task_memory
                .as_ref()
                .and_then(|selection| {
                    memory::format_task_memory_for_prompt(
                        selection,
                        phase_state.working_state.intent,
                    )
                })
        {
            let _ = append_dynamic_prompt_section(content, &mut remaining_budget, &task_memory);
        }
        if !phase_state.run_mode.is_plan_only()
            && let Some(tool_hints) =
                phase_state
                    .retrieved_task_memory
                    .as_ref()
                    .and_then(|selection| {
                        memory::format_task_tool_hints_for_prompt(
                            selection,
                            phase_state.working_state.intent,
                        )
                    })
        {
            let _ = append_dynamic_prompt_section(content, &mut remaining_budget, &tool_hints);
        }
        if !phase_state.run_mode.is_plan_only()
            && let Some(tool_order) = phase_state
                .retrieved_task_memory
                .as_ref()
                .and_then(|selection| {
                    let mut ranking = memory::task_tool_ranking_context(
                        selection,
                        phase_state.working_state.intent,
                    );
                    if let Some(task_plan) = task_plan.as_ref() {
                        ranking = merge_tool_rankings(
                            ranking,
                            agent::task_plan_tool_ranking_context(task_plan),
                        );
                    }
                    tools::render_ranked_tool_recommendations(
                        config.as_ref(),
                        latest_query.as_deref(),
                        &ranking,
                    )
                })
                .or_else(|| {
                    task_plan.as_ref().and_then(|task_plan| {
                        let ranking = agent::task_plan_tool_ranking_context(task_plan);
                        tools::render_ranked_tool_recommendations(
                            config.as_ref(),
                            latest_query.as_deref(),
                            &ranking,
                        )
                    })
                })
        {
            let _ = append_dynamic_prompt_section(content, &mut remaining_budget, &tool_order);
        }
        if !phase_state.run_mode.is_plan_only() {
            if let Some(agent_order) = crate::subagents::render_ranked_agent_recommendations(
                &discovered_agents,
                latest_query.as_deref(),
                Some(&phase_state.working_state),
            ) {
                let _ = append_dynamic_prompt_section(content, &mut remaining_budget, &agent_order);
            }
            if let Some(delegation_guidance) = crate::subagents::render_delegation_guidance(
                &discovered_agents,
                latest_query.as_deref(),
                &phase_state.working_state,
            ) {
                let _ = append_dynamic_prompt_section(
                    content,
                    &mut remaining_budget,
                    &delegation_guidance,
                );
            }
        }
        if !phase_state.run_mode.is_plan_only() && phase_state.react_ctx.cycles == 0 {
            let _ = append_dynamic_prompt_section(
                content,
                &mut remaining_budget,
                "## Working Method\n\
                 For complex multi-step tasks, use the `todos` tool to keep an ordered checklist \
                 and `think` for scratchpad reasoning before executing other tools. For simple \
                 questions or single-step tasks, respond directly.",
            );
        }
        if let Some(nudge) = agent::build_finish_nudge(phase_state.react_ctx.cycles) {
            let _ = append_dynamic_prompt_section(content, &mut remaining_budget, nudge);
        }
    }

    if let Some(first) = session.messages.first_mut()
        && first.role == "system"
    {
        *first = fresh_system;
    }

    phase_state.cycle_workspace = session.workspace.clone();

    Some(AnalyzeSnapshot {
        model: model_str,
        usage_role,
        think_level: session.think_level.clone(),
        pruned_count: 0,
        user_msg_chars,
        latest_query,
    })
}

async fn fit_messages_to_request_budget(
    ctx: &AgentRunCtx<'_>,
    model: &str,
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> Option<(usize, usize)> {
    let config = ctx.state.config();
    let provider = config.resolve_model(model).provider;
    let request_budget =
        crate::context::context_input_budget_for_runtime(&config, model, think_level);
    let message_budget = crate::context::request_message_budget_for_runtime(
        &config,
        model,
        think_level,
        extra_tools,
    );

    let pruned_count = {
        let mut sessions = ctx.state.sessions.lock().await;
        let session = sessions.get_mut(ctx.current_session_id)?;
        let before = session.messages.len();
        crate::context::prune_messages_for_provider(
            &mut session.messages,
            provider,
            message_budget,
        );
        let after = session.messages.len();
        if before != after {
            session.updated_at = now_epoch();
        }
        before.saturating_sub(after)
    };

    Some((pruned_count, request_budget))
}

async fn send_before_analyze_events(
    ctx: &AgentRunCtx<'_>,
    hook_events: Vec<serde_json::Value>,
    pruned_count: usize,
) -> Option<Vec<ChatMessage>> {
    let final_messages = {
        let sessions = ctx.state.sessions.lock().await;
        sessions
            .get(ctx.current_session_id)
            .map(|session| session.messages.clone())
            .unwrap_or_default()
    };

    for event in hook_events {
        if !live_send(ctx.live_tx, event).await {
            return None;
        }
    }

    if pruned_count > 0 {
        let _ = live_send(
            ctx.live_tx,
            json!({
                "type": "context_pruned",
                "messages_removed": pruned_count,
            }),
        )
        .await;
    }

    Some(final_messages)
}

fn runtime_auto_think_signals(
    phase_state: &AgentPhaseState,
    user_msg_chars: usize,
) -> agent::AutoThinkRuntimeSignals {
    agent::AutoThinkRuntimeSignals {
        intent: phase_state.working_state.intent,
        cycles: phase_state.react_ctx.cycles,
        observation_strength: phase_state.last_observation_strength,
        user_msg_chars,
        tool_results_count: phase_state.last_tool_results_count,
        tool_error_count: phase_state.last_tool_error_count,
        summary_count: phase_state.last_summary_count,
        summary_bytes: phase_state.last_summary_bytes,
        stagnation_streak: phase_state.stagnation_streak,
        error_streak: phase_state.error_streak,
        task_pressure: agent::auto_think_task_pressure(&phase_state.working_state),
        ready_to_finish: phase_state.working_state.ready_to_finish,
        action_oriented: matches!(
            phase_state.working_state.intent,
            agent::TaskIntent::Change | agent::TaskIntent::Investigate | agent::TaskIntent::Execute
        ),
        has_blocking_uncertainty: phase_state.working_state.has_blocking_uncertainty(),
        progress_made: phase_state.last_progress_made,
        retry_pattern: agent::auto_retry_pattern(&phase_state.recent_tool_history),
        error_kind: phase_state.last_error_kind,
        evidence_delta_quality: phase_state.last_evidence_delta_quality,
    }
}

fn reset_runtime_auto_state_for_new_goal(phase_state: &mut AgentPhaseState) {
    phase_state.react_ctx.cycles = 0;
    phase_state.last_observation_hint = None;
    phase_state.last_observation_strength = agent::AutoObservationStrength::None;
    phase_state.last_tool_results_count = 0;
    phase_state.last_tool_error_count = 0;
    phase_state.last_summary_count = 0;
    phase_state.last_summary_bytes = 0;
    phase_state.last_progress_made = false;
    phase_state.last_error_kind = agent::AutoErrorKind::None;
    phase_state.last_evidence_delta_quality = agent::AutoEvidenceDeltaQuality::None;
    phase_state.stagnation_streak = 0;
    phase_state.error_streak = 0;
    phase_state.recent_tool_history.clear();
}

fn auto_observation_strength_label(strength: agent::AutoObservationStrength) -> &'static str {
    strength.label()
}

fn record_recent_tool_result(phase_state: &mut AgentPhaseState, result: &agent::ToolResultEntry) {
    phase_state.recent_tool_history.push(result.clone());
    if phase_state.recent_tool_history.len() > AUTO_TOOL_HISTORY_CAP {
        let overflow = phase_state.recent_tool_history.len() - AUTO_TOOL_HISTORY_CAP;
        phase_state.recent_tool_history.drain(0..overflow);
    }
}

async fn build_cycle_tools(
    ctx: &AgentRunCtx<'_>,
    phase_state: &AgentPhaseState,
    resolved: &providers::ResolvedModel,
) -> Vec<serde_json::Value> {
    let config = ctx.state.config();
    if phase_state.run_mode.is_plan_only() {
        build_plan_only_tools(&config, resolved.provider, &phase_state.cycle_workspace)
    } else {
        build_runtime_tools(
            &config,
            resolved.provider,
            &phase_state.cycle_workspace,
            ctx.current_session_id,
        )
        .await
    }
}

fn tool_definition_name(value: &serde_json::Value) -> Option<&str> {
    value
        .get("function")
        .and_then(|function| function.get("name"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("name").and_then(serde_json::Value::as_str))
}

pub(crate) fn build_plan_only_tools(
    config: &Config,
    provider: Provider,
    workspace: &Path,
) -> Vec<serde_json::Value> {
    let mut definitions = tools::read_only_tool_definitions_for_provider(provider);
    let mcp_policy = tools::mcp::load_session_policy(workspace);
    let mut mcp_tools = match provider {
        Provider::Anthropic => {
            tools::mcp::cached_tool_definitions_anthropic_for_policy(config, workspace, &mcp_policy)
        }
        Provider::OpenAI | Provider::OpenAIResponses => {
            tools::mcp::cached_tool_definitions_openai_for_policy(config, workspace, &mcp_policy)
        }
        Provider::Ollama => {
            tools::mcp::cached_tool_definitions_ollama_for_policy(config, workspace, &mcp_policy)
        }
        Provider::Gemini => {
            tools::mcp::cached_tool_definitions_gemini_for_policy(config, workspace, &mcp_policy)
        }
    }
    .into_iter()
    .filter(|definition| {
        tool_definition_name(definition)
            .is_some_and(|name| tools::mcp::is_read_only_tool_name(name, config, workspace))
    })
    .collect::<Vec<_>>();
    definitions.append(&mut mcp_tools);
    definitions
}

fn available_tool_names_for_plan(config: &Config, workspace: &Path) -> Vec<String> {
    let mut names = tools::tool_specs()
        .iter()
        .map(|spec| spec.name.to_string())
        .collect::<Vec<_>>();
    let agents = crate::subagents::discovery::discover_all_agents(workspace);
    if !agents.is_empty() {
        names.push(tools::TOOL_NAME_TASK.to_string());
        names.push(tools::TOOL_NAME_ORCHESTRATE.to_string());
    }
    let mcp_policy = tools::mcp::load_session_policy(workspace);
    names.extend(
        tools::mcp::cached_list_tools_for_policy(config, workspace, &mcp_policy)
            .into_iter()
            .map(|tool| tool.exposed_name),
    );
    names.sort();
    names.dedup();
    names
}

fn available_tool_names_for_plan_only(config: &Config, workspace: &Path) -> Vec<String> {
    let mut names = tools::tool_specs()
        .iter()
        .filter(|spec| tools::is_read_only_tool(spec.name))
        .map(|spec| spec.name.to_string())
        .collect::<Vec<_>>();
    let mcp_policy = tools::mcp::load_session_policy(workspace);
    names.extend(
        tools::mcp::cached_list_tools_for_policy(config, workspace, &mcp_policy)
            .into_iter()
            .filter(tools::mcp::is_read_only_tool_descriptor)
            .map(|tool| tool.exposed_name),
    );
    names.sort();
    names.dedup();
    names
}

fn merge_tool_rankings(
    mut base: tools::ToolRankingContext,
    extra: tools::ToolRankingContext,
) -> tools::ToolRankingContext {
    for preference in extra.preferences {
        base.add_preference(
            preference.name,
            preference.reason,
            preference.score,
            preference.source,
        );
    }
    for preferred in extra.preferred_tools {
        if !base
            .preferred_tools
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&preferred))
        {
            base.preferred_tools.push(preferred);
        }
    }
    base
}

pub(crate) async fn build_runtime_tools(
    config: &Config,
    provider: Provider,
    workspace: &Path,
    current_session_id: &str,
) -> Vec<serde_json::Value> {
    let mut extra_tools = Vec::new();

    // Sub-agent task + orchestrate tools (only added when agents are discovered)
    let agents = crate::subagents::discovery::discover_all_agents(workspace);
    if !agents.is_empty() {
        let agent_names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
        let task_def = match provider {
            Provider::Anthropic => tools::task_tool_definition_anthropic(&agent_names),
            Provider::OpenAI | Provider::OpenAIResponses => {
                tools::task_tool_definition_openai(&agent_names)
            }
            Provider::Ollama => tools::task_tool_definition_ollama(&agent_names),
            Provider::Gemini => tools::task_tool_definition_gemini(&agent_names),
        };
        extra_tools.push(task_def);

        let orchestrate_def = match provider {
            Provider::Anthropic => tools::orchestrate_tool_definition_anthropic(&agent_names),
            Provider::OpenAI | Provider::OpenAIResponses => {
                tools::orchestrate_tool_definition_openai(&agent_names)
            }
            Provider::Ollama => tools::orchestrate_tool_definition_ollama(&agent_names),
            Provider::Gemini => tools::orchestrate_tool_definition_gemini(&agent_names),
        };
        extra_tools.push(orchestrate_def);
    }

    if crate::is_main(current_session_id) {
        let session_control_def = match provider {
            Provider::Anthropic => tools::session_control_tool_definition_anthropic(),
            Provider::OpenAI | Provider::OpenAIResponses => {
                tools::session_control_tool_definition_openai()
            }
            Provider::Ollama => tools::session_control_tool_definition_ollama(),
            Provider::Gemini => tools::session_control_tool_definition_gemini(),
        };
        extra_tools.push(session_control_def);
    }

    let mcp_policy = tools::mcp::load_session_policy(workspace);
    let (cached_servers, enabled_servers) =
        tools::mcp::cached_server_counts_for_policy(config, workspace, &mcp_policy);
    let mut mcp_tools = match (enabled_servers > 0, cached_servers == enabled_servers) {
        (false, _) => Vec::new(),
        (true, true) => match provider {
            Provider::Anthropic => tools::mcp::cached_tool_definitions_anthropic_for_policy(
                config,
                workspace,
                &mcp_policy,
            ),
            Provider::OpenAI | Provider::OpenAIResponses => {
                tools::mcp::cached_tool_definitions_openai_for_policy(
                    config,
                    workspace,
                    &mcp_policy,
                )
            }
            Provider::Ollama => tools::mcp::cached_tool_definitions_ollama_for_policy(
                config,
                workspace,
                &mcp_policy,
            ),
            Provider::Gemini => tools::mcp::cached_tool_definitions_gemini_for_policy(
                config,
                workspace,
                &mcp_policy,
            ),
        },
        (true, false) => match provider {
            Provider::Anthropic => {
                tools::mcp::tool_definitions_anthropic_for_policy(config, workspace, &mcp_policy)
                    .await
            }
            Provider::OpenAI | Provider::OpenAIResponses => {
                tools::mcp::tool_definitions_openai_for_policy(config, workspace, &mcp_policy).await
            }
            Provider::Ollama => {
                tools::mcp::tool_definitions_ollama_for_policy(config, workspace, &mcp_policy).await
            }
            Provider::Gemini => {
                tools::mcp::tool_definitions_gemini_for_policy(config, workspace, &mcp_policy).await
            }
        },
    };
    extra_tools.append(&mut mcp_tools);
    extra_tools
}

fn token_usage_source(token_count: Option<u64>) -> &'static str {
    if token_count.is_some() {
        "provider"
    } else {
        "estimated"
    }
}

async fn update_llm_response_usage(
    ctx: &AgentRunCtx<'_>,
    resolved_provider: Provider,
    provider_name: &str,
    usage_role: &str,
    request_input_estimate: u64,
    resp: &providers::LlmResponse,
) {
    let input_tokens = resp.input_tokens.unwrap_or(request_input_estimate);
    let output_tokens = resp
        .output_tokens
        .unwrap_or_else(|| message_token_len_for_provider(resolved_provider, &resp.message) as u64);

    let mut sessions = ctx.state.sessions.lock().await;
    if let Some(session) = sessions.get_mut(ctx.current_session_id) {
        crate::update_session_token_usage_with_provider(
            session,
            input_tokens,
            output_tokens,
            token_usage_source(resp.input_tokens),
            token_usage_source(resp.output_tokens),
            Some(provider_name),
            Some(usage_role),
        );
    }
}

async fn persist_assistant_message(ctx: &AgentRunCtx<'_>, message: &ChatMessage) {
    if message.is_empty_assistant_message() {
        return;
    }

    let mut sanitized_message = message.clone();
    tools::sanitize_chat_message_tool_calls_in_place(&mut sanitized_message);

    let mut sessions = ctx.state.sessions.lock().await;
    if let Some(session) = sessions.get_mut(ctx.current_session_id) {
        session.messages.push(sanitized_message);
        session.updated_at = now_epoch();
    }
}

async fn advance_after_llm_response(
    live_tx: &LiveTx,
    phase_state: &mut AgentPhaseState,
    message: &ChatMessage,
    latest_query: Option<&str>,
) {
    let has_content = message.has_nonempty_content();
    let has_tools = message.has_tool_calls();
    match agent::evaluate_finish(has_content, has_tools) {
        None => {
            phase_state.results_origin_query = latest_query.map(str::to_string);
            phase_state.pending_tool_calls = message.tool_calls.clone().unwrap_or_default();
            phase_state.react_ctx.transition_to_act();
            send_react_phase_event(live_tx, &phase_state.react_ctx, "act").await;
        }
        Some(reason) => {
            phase_state.results_origin_query = None;
            phase_state.react_ctx.transition_to_finish(reason);
            send_react_phase_event(live_tx, &phase_state.react_ctx, "finish").await;
        }
    }
    phase_state.round += 1;
}

#[allow(clippy::too_many_arguments)]
async fn apply_llm_response(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
    resolved_provider: Provider,
    provider_name: String,
    usage_role: &'static str,
    request_input_estimate: u64,
    latest_query: Option<&str>,
    resp: providers::LlmResponse,
) {
    update_llm_response_usage(
        ctx,
        resolved_provider,
        &provider_name,
        usage_role,
        request_input_estimate,
        &resp,
    )
    .await;
    persist_assistant_message(ctx, &resp.message).await;
    advance_after_llm_response(ctx.live_tx, phase_state, &resp.message, latest_query).await;
}

async fn execute_tool(
    name: &str,
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
    isolated_mcp_session: bool,
    event_tx: Option<tools::ToolEventSender>,
) -> tools::ToolOutcome {
    let mcp_policy = tools::mcp::load_session_policy(workspace);
    let mcp_result = tools::mcp::execute_tool_for_policy(
        name,
        args_str,
        config,
        workspace,
        isolated_mcp_session,
        &mcp_policy,
    )
    .await;

    if let Some(result) = mcp_result {
        result
    } else {
        tools::execute_tool(name, args_str, config, http, workspace, event_tx).await
    }
}

async fn execute_todos_tool(
    state: &Arc<AppState>,
    session_id: &str,
    args_str: &str,
) -> tools::ToolOutcome {
    let state = Arc::clone(state);
    let session_id = session_id.to_string();
    let args_str = args_str.to_string();

    // Keep the durable write path alive even if the outer tool loop is cancelled
    // after we've started mutating session-scoped todo state.
    match tokio::spawn(async move {
        let start = std::time::Instant::now();
        let request: crate::todos::TodoReplaceRequest = match serde_json::from_str(&args_str) {
            Ok(request) => request,
            Err(error) => {
                return tools::ToolOutcome {
                    output: format!("todos error: invalid arguments JSON: {error}"),
                    is_error: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                    subagent_snapshot: None,
                };
            }
        };

        match crate::todos::replace_session_todos(
            state.as_ref(),
            &session_id,
            request,
            crate::todos::TodoUpdateOrigin::Assistant,
        )
        .await
        {
            Ok(response) => tools::ToolOutcome {
                output: serde_json::to_string(&response)
                    .unwrap_or_else(|_| "{\"ok\":false,\"conflict\":false}".to_string()),
                is_error: false,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            },
            Err(error) => tools::ToolOutcome {
                output: error.message(),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            },
        }
    })
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => tools::ToolOutcome {
            output: format!("todos error: internal task failed: {error}"),
            is_error: true,
            duration_ms: 0,
            subagent_snapshot: None,
        },
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_tool_with_live_output(
    live_tx: &LiveTx,
    tool_id: &str,
    name: &str,
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
    isolated_mcp_session: bool,
    replay_ctx: Option<crate::LiveOutputReplayCtx>,
) -> tools::ToolOutcome {
    if name != tools::TOOL_NAME_EXEC {
        return execute_tool(
            name,
            args_str,
            config,
            http,
            workspace,
            isolated_mcp_session,
            None,
        )
        .await;
    }

    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::channel(tools::TOOL_LIVE_EVENT_CHANNEL_CAPACITY);
    let tool_future = tools::execute_tool_with_bounded_live_events(
        name,
        args_str,
        config,
        http,
        workspace,
        None,
        Some(event_tx),
    );
    tokio::pin!(tool_future);
    let mut forward_event = |stream, chunk: String| {
        let replay_ctx = replay_ctx.clone();
        async move {
            crate::forward_tool_output_event_best_effort(
                live_tx,
                json!({
                    "type": "tool_output",
                    "id": tool_id,
                    "name": name,
                    "stream": stream,
                    "chunk": chunk,
                }),
                replay_ctx.as_ref(),
            )
            .await;
        }
    };
    let mut pending_result: Option<tools::ToolOutcome> = None;
    let mut event_rx_open = true;

    loop {
        if let Some(result) = pending_result.take() {
            if event_rx_open {
                tools::drain_bounded_exec_live_events(&mut event_rx, &mut forward_event).await;
            }
            return result;
        }

        if !event_rx_open {
            return tool_future.as_mut().await;
        }

        tokio::select! {
            biased;
            result = &mut tool_future => {
                pending_result = Some(result);
            }
            maybe_event = event_rx.recv() => {
                match maybe_event {
                    Some(event) => {
                        tools::forward_exec_live_event(event, &mut forward_event).await;
                    }
                    None => {
                        event_rx_open = false;
                    }
                }
            }
        }
    }
}

/// Execute a `task` tool call by delegating to a sub-agent.
/// Returns the outcome as a standard ToolOutcome so it integrates with the
/// existing record_tool_result flow. Sub-agent token usage is accumulated into
/// the parent session counters so global/daily stats remain accurate.
#[allow(clippy::too_many_arguments)]
async fn execute_task_tool(
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
    live_tx: &LiveTx,
    cancel: CancellationToken,
    hooks: &HookRegistry,
    state: &Arc<AppState>,
    session_id: &str,
) -> tools::ToolOutcome {
    let start = std::time::Instant::now();

    let args: serde_json::Value = match serde_json::from_str(args_str) {
        Ok(v) => v,
        Err(e) => {
            return tools::ToolOutcome {
                output: format!("task error: invalid arguments JSON: {e}"),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            };
        }
    };

    // Validate task tool parameters against schema
    if let Some(err) = tools::validate_tool_args("task", &args, &tools::task_tool_parameters()) {
        return tools::ToolOutcome {
            output: err,
            is_error: true,
            duration_ms: start.elapsed().as_millis() as u64,
            subagent_snapshot: None,
        };
    }

    let agent_name = match args.get("agent").and_then(|v| v.as_str()) {
        Some(name) => name,
        None => {
            return tools::ToolOutcome {
                output: "task error: missing required parameter 'agent'".to_string(),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            };
        }
    };

    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return tools::ToolOutcome {
                output: "task error: missing required parameter 'prompt'".to_string(),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            };
        }
    };
    let effective_prompt =
        crate::subagents::executor::augment_subagent_prompt_with_current_time(prompt);

    let spec = match crate::subagents::discovery::find_agent(workspace, agent_name) {
        Some(s) => s,
        None => {
            let available = crate::subagents::discovery::discover_all_agents(workspace);
            let names: Vec<&str> = available.iter().map(|a| a.name.as_str()).collect();
            return tools::ToolOutcome {
                output: format!(
                    "task error: sub-agent '{}' not found. Available agents: {}",
                    agent_name,
                    if names.is_empty() {
                        "(none)".to_string()
                    } else {
                        names.join(", ")
                    }
                ),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            };
        }
    };

    // Generate a unique task_id so the frontend can key parallel same-agent
    // task panels independently. 8 bytes = 16 hex chars, ample for a session.
    let task_id = {
        let mut bytes = [0u8; 8];
        if getrandom::getrandom(&mut bytes).is_ok() {
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        } else {
            let seq = NEXT_FALLBACK_TASK_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(seq as u128);
            format!("task-{nanos:x}-{seq:x}")
        }
    };

    // Send task_started event
    let _ = live_send(
        live_tx,
        json!({
            "type": "task_started",
            "task_id": task_id,
            "agent": agent_name,
            "prompt": crate::truncate(prompt, 500),
        }),
    )
    .await;

    // Guard ensures task_failed is sent if we're dropped after task_started
    // (e.g. timeout or cancellation in run_tool_with_feedback).
    let mut guard = TaskEventGuard::new(live_tx, agent_name, &task_id);

    let outcome = crate::subagents::executor::run_subagent(
        &spec,
        &effective_prompt,
        config,
        http,
        workspace,
        live_tx,
        cancel,
        hooks,
        Some(crate::LiveOutputReplayCtx {
            state: Arc::clone(state),
            session_id: session_id.to_string(),
        }),
        &task_id,
    )
    .await;

    let duration_ms = start.elapsed().as_millis() as u64;

    // Propagate sub-agent token usage into the parent session so stats reflect
    // the full cost of delegation.  The executor mixes provider-reported and
    // locally-estimated counts (prefer provider, fall back to estimate), so
    // the source label is conservatively "estimated".
    if outcome.total_input_tokens > 0 || outcome.total_output_tokens > 0 {
        let mut usage_labels = outcome.provider_usage.clone();
        usage_labels.extend(crate::context::build_usage_labels(
            outcome.total_input_tokens,
            outcome.total_output_tokens,
            None,
            Some(crate::context::USAGE_ROLE_SUB_AGENT),
        ));
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            crate::update_session_token_usage_with_providers(
                session,
                outcome.total_input_tokens,
                outcome.total_output_tokens,
                "estimated",
                "estimated",
                &usage_labels,
            );
        }
    }

    // Send task_completed / task_failed event
    let terminal_event = if outcome.aborted {
        json!({
            "type": "task_failed",
            "task_id": task_id,
            "agent": agent_name,
            "error": outcome.result,
            "cycles": outcome.cycles,
            "tool_calls": outcome.tool_calls,
            "input_tokens": outcome.total_input_tokens,
            "output_tokens": outcome.total_output_tokens,
            "duration_ms": duration_ms,
        })
    } else {
        json!({
            "type": "task_completed",
            "task_id": task_id,
            "agent": agent_name,
            "cycles": outcome.cycles,
            "tool_calls": outcome.tool_calls,
            "input_tokens": outcome.total_input_tokens,
            "output_tokens": outcome.total_output_tokens,
            "duration_ms": duration_ms,
            "result_preview": crate::truncate(&outcome.result, 400),
            "result_excerpt": crate::truncate(&outcome.result, 4_000),
        })
    };
    let _ = live_send(live_tx, terminal_event).await;
    guard.mark_finished();

    let mut history_snapshot = outcome.history_snapshot;
    history_snapshot.duration_ms = duration_ms;
    if outcome.aborted {
        history_snapshot.error = Some(crate::truncate(&outcome.result, 4_000).to_string());
        history_snapshot.result_excerpt = None;
    } else {
        history_snapshot.result_excerpt = Some(crate::truncate(&outcome.result, 4_000).to_string());
        history_snapshot.error = None;
    }

    tools::ToolOutcome {
        output: outcome.result,
        is_error: outcome.aborted,
        duration_ms,
        subagent_snapshot: Some(history_snapshot),
    }
}

/// Execute an `orchestrate` tool call by coordinating multiple sub-agents.
/// Returns the outcome as a standard ToolOutcome so it integrates with the
/// existing record_tool_result flow. Aggregated sub-agent token usage is
/// written back to the parent session for accurate stats tracking.
#[allow(clippy::too_many_arguments)]
async fn execute_orchestrate_tool(
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
    live_tx: &LiveTx,
    cancel: CancellationToken,
    hooks: &HookRegistry,
    state: &Arc<AppState>,
    session_id: &str,
) -> tools::ToolOutcome {
    let start = std::time::Instant::now();

    let args: serde_json::Value = match serde_json::from_str(args_str) {
        Ok(v) => v,
        Err(e) => {
            return tools::ToolOutcome {
                output: format!("orchestrate error: invalid arguments JSON: {e}"),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            };
        }
    };

    // Validate against schema
    if let Some(err) =
        tools::validate_tool_args("orchestrate", &args, &tools::orchestrate_tool_parameters())
    {
        return tools::ToolOutcome {
            output: err,
            is_error: true,
            duration_ms: start.elapsed().as_millis() as u64,
            subagent_snapshot: None,
        };
    }

    // Parse tasks array
    let tasks: Vec<crate::subagents::orchestrator::OrchestrationTask> = match args
        .get("tasks")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
    {
        Some(t) => t,
        None => {
            return tools::ToolOutcome {
                output: "orchestrate error: missing or invalid 'tasks' array".to_string(),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            };
        }
    };

    // Validate plan (IDs, agents, dependencies, cycles)
    let plan = match crate::subagents::orchestrator::validate_plan(tasks, workspace) {
        Ok(p) => p,
        Err(e) => {
            return tools::ToolOutcome {
                output: e,
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            };
        }
    };

    let outcome = crate::subagents::orchestrator::execute_orchestration(
        &plan,
        config,
        http,
        workspace,
        live_tx,
        cancel,
        hooks,
        Some(crate::LiveOutputReplayCtx {
            state: Arc::clone(state),
            session_id: session_id.to_string(),
        }),
    )
    .await;

    let duration_ms = start.elapsed().as_millis() as u64;
    let result = crate::subagents::orchestrator::format_orchestration_result(&outcome);

    // Propagate aggregated sub-agent token usage into the parent session so
    // the user-facing stats and daily totals include the cost of delegation.
    // Inner executors mix provider-reported and estimated counts, so the
    // source label is conservatively "estimated".
    let input_tokens = outcome.total_input_tokens();
    let output_tokens = outcome.total_output_tokens();
    let provider_usage = outcome.provider_usage();
    if input_tokens > 0 || output_tokens > 0 {
        let mut usage_labels = provider_usage.clone();
        usage_labels.extend(crate::context::build_usage_labels(
            input_tokens,
            output_tokens,
            None,
            Some(crate::context::USAGE_ROLE_SUB_AGENT),
        ));
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(session_id) {
            crate::update_session_token_usage_with_providers(
                session,
                input_tokens,
                output_tokens,
                "estimated",
                "estimated",
                &usage_labels,
            );
        }
    }

    tools::ToolOutcome {
        output: result,
        is_error: outcome.aborted || outcome.has_non_completed_tasks(),
        duration_ms,
        subagent_snapshot: None,
    }
}

fn build_agent_hard_cap_events(
    round_limit: usize,
    cycles: usize,
    tool_calls: usize,
) -> (serde_json::Value, serde_json::Value) {
    (
        json!({
            "type": "system",
            "content": format!(
                "Detected abnormal tool loop ({} consecutive rounds). Stopping.",
                round_limit
            ),
        }),
        json!({
            "type": "done",
            "phase": "hard_cap",
            "reason": "hard_cap",
            "cycles": cycles,
            "tool_calls": tool_calls,
        }),
    )
}

/// Read session token counters and compute round deltas for the `done` event.
async fn build_done_usage(
    state: &AppState,
    session_id: &str,
    snap_input: u64,
    snap_output: u64,
) -> serde_json::Value {
    let sessions = state.sessions.lock().await;
    if let Some(s) = sessions.get(session_id) {
        let (daily_in, daily_out) = context::current_daily_token_usage(s);
        json!({
            "daily_input_tokens": daily_in,
            "daily_output_tokens": daily_out,
            "total_input_tokens": s.input_tokens,
            "total_output_tokens": s.output_tokens,
            "round_input_tokens": s.input_tokens.saturating_sub(snap_input),
            "round_output_tokens": s.output_tokens.saturating_sub(snap_output),
        })
    } else {
        json!({})
    }
}

async fn run_tool_with_feedback<F>(
    live_tx: &LiveTx,
    cancel: &CancellationToken,
    tool_id: &str,
    tool_name: &str,
    timeout: Option<Duration>,
    future: F,
) -> ToolRunState
where
    F: std::future::Future<Output = tools::ToolOutcome>,
{
    let start = std::time::Instant::now();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(TOOL_PROGRESS_HEARTBEAT_SECS));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    heartbeat.tick().await;

    let has_timeout = timeout.is_some();
    let timeout_secs = timeout.map(|t| t.as_secs()).unwrap_or(0);
    let sleep = tokio::time::sleep(timeout.unwrap_or(Duration::ZERO));
    tokio::pin!(sleep);
    tokio::pin!(future);

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return ToolRunState::Abort;
            }
            _ = &mut sleep, if has_timeout => {
                return ToolRunState::Completed(tools::ToolOutcome {
                    output: format!("{tool_name} error: tool execution timed out ({}s)", timeout_secs),
                    is_error: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                    subagent_snapshot: None,
                });
            }
            _ = heartbeat.tick() => {
                if !live_send(
                    live_tx,
                    json!({
                        "type": "tool_progress",
                        "id": tool_id,
                        "name": tool_name,
                        "elapsed_ms": start.elapsed().as_millis() as u64,
                    }),
                )
                .await
                {
                    return ToolRunState::Abort;
                }
            }
            result = &mut future => {
                return ToolRunState::Completed(result);
            }
        }
    }
}

fn runtime_timeout_for_tool(tool_name: &str, config: &Config) -> Option<Duration> {
    tools::tool_runtime_timeout(tool_name, config)
}

fn is_plan_only_allowed_tool(tool_name: &str, config: &Config, workspace: &Path) -> bool {
    if tools::is_read_only_tool(tool_name) {
        return true;
    }

    let mcp_policy = tools::mcp::load_session_policy(workspace);
    tools::mcp::is_read_only_tool_name_for_policy(tool_name, config, workspace, &mcp_policy)
}

/// Returns `(outcome, effective_args)` where `effective_args` is `None` when
/// the tool was rejected by a BeforeToolExec hook (signals record_tool_result
/// to skip AfterToolExec), or `Some(args_json)` with the actually-executed args.
async fn execute_tool_call(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
    tc: &ToolCall,
) -> Result<(tools::ToolOutcome, Option<String>), AgentPhaseControl> {
    let config = ctx.state.config();
    if phase_state.run_mode.is_plan_only()
        && !is_plan_only_allowed_tool(&tc.function.name, &config, &phase_state.cycle_workspace)
    {
        if !tools::is_todos_tool(&tc.function.name) {
            let display_args =
                tools::display_tool_arguments(&tc.function.name, &tc.function.arguments);
            let _ = live_send(
                ctx.live_tx,
                json!({
                    "type":"tool_call",
                    "id": tc.id,
                    "name": tc.function.name,
                    "arguments": display_args,
                }),
            )
            .await;
        }
        return Ok((
            tools::ToolOutcome {
                output: format!(
                    "[rejected by plan mode] `{}` is not available while planning. Produce the plan without executing mutating tools.",
                    tc.function.name
                ),
                is_error: true,
                duration_ms: 0,
                subagent_snapshot: None,
            },
            None,
        ));
    }

    // ── BeforeToolExec hook (evaluated before the WS event so the frontend
    //    always sees the arguments that will actually be executed) ─────────
    let tool_hook_input = ToolHookInput {
        tool_name: tc.function.name.clone(),
        tool_args: serde_json::from_str(&tc.function.arguments)
            .unwrap_or_else(|_| serde_json::Value::String(tc.function.arguments.clone())),
        tool_id: tc.id.clone(),
        cycle: phase_state.react_ctx.cycles,
        workspace: phase_state.cycle_workspace.clone(),
        outcome_output: None,
        outcome_is_error: None,
        outcome_duration_ms: None,
    };
    let hook_output = run_tool_hooks(
        &ctx.state.hooks,
        agent::HookPoint::BeforeToolExec,
        tool_hook_input,
        &config,
    )
    .await;

    let effective_args = match hook_output {
        hooks::HookOutput::Reject { reason, events } => {
            // Still send the tool_call event so the frontend sees the attempted call.
            if !tools::is_todos_tool(&tc.function.name) {
                let display_args =
                    tools::display_tool_arguments(&tc.function.name, &tc.function.arguments);
                let _ = live_send(
                    ctx.live_tx,
                    json!({
                        "type":"tool_call",
                        "id": tc.id,
                        "name": tc.function.name,
                        "arguments": display_args,
                    }),
                )
                .await;
            }
            for ev in events {
                let _ = live_send(ctx.live_tx, ev).await;
            }
            return Ok((
                tools::ToolOutcome {
                    output: format!("[rejected by hook] {reason}"),
                    is_error: true,
                    duration_ms: 0,
                    subagent_snapshot: None,
                },
                None, // rejected — skip AfterToolExec
            ));
        }
        hooks::HookOutput::ModifyToolArgs { args } => {
            serde_json::to_string(&args).unwrap_or_else(|_| tc.function.arguments.clone())
        }
        _ => tc.function.arguments.clone(),
    };
    let display_args = tools::display_tool_arguments(&tc.function.name, &effective_args);

    // Send tool_call event with the effective (possibly hook-modified) arguments.
    if !tools::is_todos_tool(&tc.function.name)
        && !live_send(
            ctx.live_tx,
            json!({
                "type":"tool_call",
                "id": tc.id,
                "name": tc.function.name,
                "arguments": display_args,
            }),
        )
        .await
    {
        return Err(AgentPhaseControl::Break);
    }

    let run_state = if tools::is_task_tool(&tc.function.name) {
        // Sub-agent task: no outer timeout — the sub-agent enforces its own
        // deadline via config.sub_agent_timeout inside run_subagent().
        let task_cancel = ctx.run_cancel.child_token();
        run_tool_with_feedback(
            ctx.live_tx,
            ctx.run_cancel,
            &tc.id,
            &tc.function.name,
            None,
            execute_task_tool(
                &effective_args,
                &config,
                &ctx.state.http,
                &phase_state.cycle_workspace,
                ctx.live_tx,
                task_cancel,
                &ctx.state.hooks,
                ctx.state,
                ctx.current_session_id,
            ),
        )
        .await
    } else if tools::is_orchestrate_tool(&tc.function.name) {
        // Multi-agent orchestration: no outer timeout — individual sub-agents
        // enforce their own deadlines via config.sub_agent_timeout.
        let orch_cancel = ctx.run_cancel.child_token();
        run_tool_with_feedback(
            ctx.live_tx,
            ctx.run_cancel,
            &tc.id,
            &tc.function.name,
            None,
            execute_orchestrate_tool(
                &effective_args,
                &config,
                &ctx.state.http,
                &phase_state.cycle_workspace,
                ctx.live_tx,
                orch_cancel,
                &ctx.state.hooks,
                ctx.state,
                ctx.current_session_id,
            ),
        )
        .await
    } else if tools::is_session_control_tool(&tc.function.name) {
        if phase_state.run_mode.is_plan_only() {
            return Ok((
                tools::ToolOutcome {
                    output: "[rejected by plan mode] `session_control` is not available while planning. Produce the plan without controlling other sessions.".to_string(),
                    is_error: true,
                    duration_ms: 0,
                    subagent_snapshot: None,
                },
                None,
            ));
        }
        run_tool_with_feedback(
            ctx.live_tx,
            ctx.run_cancel,
            &tc.id,
            &tc.function.name,
            None,
            crate::session_control::execute_session_control_tool(
                ctx.state,
                ctx.current_session_id,
                &effective_args,
            ),
        )
        .await
    } else if tools::is_todos_tool(&tc.function.name) {
        run_tool_with_feedback(
            ctx.live_tx,
            ctx.run_cancel,
            &tc.id,
            &tc.function.name,
            runtime_timeout_for_tool(&tc.function.name, &config),
            execute_todos_tool(ctx.state, ctx.current_session_id, &effective_args),
        )
        .await
    } else {
        run_tool_with_feedback(
            ctx.live_tx,
            ctx.run_cancel,
            &tc.id,
            &tc.function.name,
            runtime_timeout_for_tool(&tc.function.name, &config),
            execute_tool_with_live_output(
                ctx.live_tx,
                &tc.id,
                &tc.function.name,
                &effective_args,
                &config,
                &ctx.state.http,
                &phase_state.cycle_workspace,
                false,
                Some(crate::LiveOutputReplayCtx {
                    state: Arc::clone(ctx.state),
                    session_id: ctx.current_session_id.to_string(),
                }),
            ),
        )
        .await
    };

    match run_state {
        ToolRunState::Completed(result) => Ok((result, Some(effective_args))),
        ToolRunState::Abort => {
            apply_run_cancel_outcome(ctx, phase_state).await;
            Err(AgentPhaseControl::Break)
        }
    }
}

/// `effective_args`: `Some(args_json)` = the args actually executed (used for
/// AfterToolExec hook input); `None` = tool was rejected by BeforeToolExec —
/// AfterToolExec hooks are skipped entirely.
async fn record_tool_result(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
    tc: &ToolCall,
    mut result: tools::ToolOutcome,
    effective_args: Option<&str>,
) -> AgentPhaseControl {
    // ── AfterToolExec hook (skipped when tool was rejected) ──────────────
    let config = ctx.state.config();
    if let Some(eff_args) = effective_args {
        let after_input = ToolHookInput {
            tool_name: tc.function.name.clone(),
            tool_args: serde_json::from_str(eff_args)
                .unwrap_or_else(|_| serde_json::Value::String(eff_args.to_string())),
            tool_id: tc.id.clone(),
            cycle: phase_state.react_ctx.cycles,
            workspace: phase_state.cycle_workspace.clone(),
            outcome_output: Some(result.output.clone()),
            outcome_is_error: Some(result.is_error),
            outcome_duration_ms: Some(result.duration_ms),
        };
        let hook_output = run_tool_hooks(
            &ctx.state.hooks,
            agent::HookPoint::AfterToolExec,
            after_input,
            &config,
        )
        .await;
        if let hooks::HookOutput::ModifyToolResult { result: new_output } = hook_output {
            result.output = new_output;
        }
    }

    if !tools::is_todos_tool(&tc.function.name)
        && !live_send(
            ctx.live_tx,
            json!({
                "type":"tool_result",
                "id": tc.id,
                "name": tc.function.name,
                "result": result.output,
                "duration_ms": result.duration_ms,
                "is_error": result.is_error,
            }),
        )
        .await
    {
        return AgentPhaseControl::Break;
    }

    let trace = tools::build_tool_execution_trace(&tc.function.name, effective_args);
    let call_summary = trace
        .as_ref()
        .and_then(agent::ToolExecutionTrace::summary)
        .map(str::to_string);

    let result_entry = agent::ToolResultEntry {
        id: tc.id.clone(),
        name: tc.function.name.clone(),
        duration_ms: result.duration_ms,
        is_error: result.is_error,
        result: result.output.clone(),
        call_summary,
        trace,
    };
    record_recent_tool_result(phase_state, &result_entry);
    phase_state.collected_results.push(result_entry);

    {
        let mut sessions = ctx.state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(ctx.current_session_id) {
            if result.is_error {
                session.failed_tool_results.insert(tc.id.clone());
            } else {
                session.failed_tool_results.remove(&tc.id);
            }
            if let Some(snapshot) = result.subagent_snapshot.take() {
                let occurrence = session
                    .messages
                    .iter()
                    .filter(|message| {
                        message.role == "tool"
                            && message.tool_call_id.as_deref() == Some(tc.id.as_str())
                    })
                    .count()
                    + 1;
                let mut sanitized_snapshot = snapshot;
                tools::sanitize_subagent_snapshot_tool_args_in_place(&mut sanitized_snapshot);
                session.subagent_snapshots.insert(
                    session_store::subagent_snapshot_storage_key(&tc.id, occurrence),
                    sanitized_snapshot,
                );
            } else {
                let occurrence = session
                    .messages
                    .iter()
                    .filter(|message| {
                        message.role == "tool"
                            && message.tool_call_id.as_deref() == Some(tc.id.as_str())
                    })
                    .count()
                    + 1;
                session
                    .subagent_snapshots
                    .remove(&session_store::subagent_snapshot_storage_key(
                        &tc.id, occurrence,
                    ));
            }
            session.messages.push(ChatMessage {
                role: "tool".into(),
                content: Some(result.output),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
                timestamp: Some(now_epoch()),
            });
            session.tool_calls_count += 1;
        }
    }

    AgentPhaseControl::Continue
}

#[cfg(test)]
fn summarize_effective_tool_args(tool_name: &str, effective_args: Option<&str>) -> Option<String> {
    tools::build_tool_execution_trace(tool_name, effective_args)
        .and_then(|trace| trace.summary().map(str::to_string))
}

async fn finish_act_phase(live_tx: &LiveTx, phase_state: &mut AgentPhaseState, tc_count: usize) {
    phase_state.react_ctx.transition_to_observe(tc_count);
    send_react_phase_event(live_tx, &phase_state.react_ctx, "observe").await;
}

async fn update_working_state(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
    summaries: &[agent::ObservationSummary],
) {
    let config = ctx.state.config();
    let session_data = {
        let sessions = ctx.state.sessions.lock().await;
        sessions
            .get(ctx.current_session_id)
            .map(|session| WorkingStateSessionData {
                latest_query: latest_user_query_from_messages(&session.messages),
                workspace: session.workspace.clone(),
                fallback_model: session.effective_model(&config.model).to_string(),
            })
    };

    let latest_query = session_data
        .as_ref()
        .and_then(|session| session.latest_query.clone());
    let working_query = phase_state
        .results_origin_query
        .clone()
        .or(latest_query.clone());
    phase_state
        .working_state
        .seed_from_query(working_query.as_deref());
    if let Some(session) = session_data.as_ref() {
        if !phase_state.collected_results.is_empty() {
            refresh_retrieved_task_memory(
                phase_state,
                &session.workspace,
                config.structured_memory,
                working_query.as_deref(),
                true,
            );
        }
    } else {
        phase_state.retrieved_task_memory = None;
        phase_state.retrieved_task_memory_key = None;
        phase_state.retrieved_task_memory_cycle = None;
    }

    agent::apply_rule_based_working_state_update_with_memory(
        &mut phase_state.working_state,
        &phase_state.collected_results,
        phase_state.retrieved_task_memory.as_ref(),
    );

    if !phase_state.collected_results.is_empty()
        && let Some(session) = session_data.as_ref()
    {
        refresh_retrieved_task_memory(
            phase_state,
            &session.workspace,
            config.structured_memory,
            working_query.as_deref(),
            false,
        );
    }

    if !agent::should_trigger_state_digest(&phase_state.collected_results) {
        return;
    }

    let Some(session) = session_data else {
        return;
    };

    if config.enable_state_digest {
        let model = config.fast_model.clone().unwrap_or(session.fallback_model);

        if let Some(delta) = summarize_working_state_with_llm(
            ctx,
            config.as_ref(),
            &phase_state.working_state,
            working_query.as_deref(),
            summaries,
            &phase_state.collected_results,
            phase_state.retrieved_task_memory.as_ref(),
            &model,
            &session.workspace,
        )
        .await
        {
            agent::merge_state_digest_delta(&mut phase_state.working_state, delta);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn summarize_working_state_with_llm(
    ctx: &AgentRunCtx<'_>,
    config: &Config,
    state: &agent::WorkingState,
    latest_query: Option<&str>,
    summaries: &[agent::ObservationSummary],
    results: &[agent::ToolResultEntry],
    task_memory: Option<&memory::RetrievedTaskMemory>,
    model: &str,
    workspace: &std::path::Path,
) -> Option<agent::StateDigestDelta> {
    let user_prompt = build_working_state_digest_user_prompt(
        state,
        latest_query,
        summaries,
        results,
        task_memory,
    )?;

    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some(
                "You are updating an agent working state after tool observations.\n\
                 Return ONLY valid JSON matching this schema:\n\
                 {\"completed_steps\":[\"...\"],\"evidence_add\":[{\"claim\":\"...\",\"source_tool\":\"...\",\"source_ref\":\"...\",\"confidence\":\"Low|Medium|High\"}],\"open_questions\":[\"...\"],\"uncertainties_add\":[{\"topic\":\"...\",\"reason\":\"...\",\"blocking\":true}],\"next_actions\":[\"...\"],\"ready_to_finish\":false}\n\
                 Rules:\n\
                 - Keep items concise and specific.\n\
                 - Only add new evidence or unresolved uncertainties.\n\
                 - Prefer High confidence for direct tool findings, Medium for strong inferences, Low for tentative ideas.\n\
                 - Use the same language as the conversation.\n\
                 - Do not include markdown fences or explanations."
                    .into(),
            ),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(user_prompt),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let resolved = config.resolve_model(model);
    let response = match tokio::time::timeout(
        std::time::Duration::from_secs(15),
        providers::call_llm_simple_with_usage(
            &ctx.state.http,
            &resolved,
            &messages,
            workspace,
            config.s3.as_ref(),
            "off",
            config.max_llm_retries,
        ),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            report_working_state_digest_issue(
                ctx.live_tx,
                model,
                WorkingStateDigestIssue::ProviderError(error.to_string()),
            )
            .await;
            return None;
        }
        Err(_) => {
            report_working_state_digest_issue(ctx.live_tx, model, WorkingStateDigestIssue::Timeout)
                .await;
            return None;
        }
    };

    let provider_name = config.resolve_provider_name(model);
    let input_tokens = response.input_tokens.unwrap_or_else(|| {
        crate::estimate_tokens_for_provider(resolved.provider, &messages) as u64
    });
    let output_tokens = response.output_tokens.unwrap_or_else(|| {
        crate::message_token_len_for_provider(
            resolved.provider,
            &ChatMessage {
                role: "assistant".into(),
                content: Some(response.content.clone()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
        ) as u64
    });
    {
        let mut sessions = ctx.state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(ctx.current_session_id) {
            crate::update_session_token_usage_with_provider(
                session,
                input_tokens,
                output_tokens,
                token_usage_source(response.input_tokens),
                token_usage_source(response.output_tokens),
                Some(&provider_name),
                Some(crate::context::USAGE_ROLE_CONTEXT),
            );
        }
    }

    let content = crate::strip_json_fences(response.content.trim());
    match serde_json::from_str::<agent::StateDigestDelta>(content) {
        Ok(delta) => Some(delta),
        Err(error) => {
            report_working_state_digest_issue(
                ctx.live_tx,
                model,
                WorkingStateDigestIssue::InvalidJson(format!(
                    "{error}; content={}",
                    crate::truncate(content, 200)
                )),
            )
            .await;
            None
        }
    }
}

async fn report_working_state_digest_issue(
    live_tx: &LiveTx,
    model: &str,
    issue: WorkingStateDigestIssue,
) {
    let (reason, detail, content) = match issue {
        WorkingStateDigestIssue::ProviderError(error) => (
            "provider_error",
            Some(error.clone()),
            format!(
                "Working state digest failed on `{model}`; continuing with rule-based state tracking."
            ),
        ),
        WorkingStateDigestIssue::Timeout => (
            "timeout",
            None,
            format!(
                "Working state digest timed out on `{model}`; continuing with rule-based state tracking."
            ),
        ),
        WorkingStateDigestIssue::InvalidJson(error) => (
            "invalid_json",
            Some(error.clone()),
            format!(
                "Working state digest returned invalid JSON on `{model}`; continuing with rule-based state tracking."
            ),
        ),
    };

    if let Some(detail) = detail.as_deref() {
        eprintln!("Working state digest {reason} (non-critical): {detail}");
    } else {
        eprintln!("Working state digest {reason} (non-critical)");
    }

    let mut event = json!({
        "type": "system",
        "level": "warning",
        "source": "working_state_digest",
        "reason": reason,
        "model": model,
        "content": content,
    });
    if let Some(detail) = detail {
        event["detail"] = serde_json::Value::String(crate::truncate(&detail, 240));
    }
    let _ = live_send(live_tx, event).await;
}

fn build_working_state_digest_user_prompt(
    state: &agent::WorkingState,
    latest_query: Option<&str>,
    summaries: &[agent::ObservationSummary],
    results: &[agent::ToolResultEntry],
    task_memory: Option<&memory::RetrievedTaskMemory>,
) -> Option<String> {
    let state_json = serde_json::to_string_pretty(state).ok()?;
    let summary_text = if summaries.is_empty() {
        "none".to_string()
    } else {
        summaries
            .iter()
            .map(|summary| format!("- {}: {}", summary.tool_name, summary.hint))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let result_text = results
        .iter()
        .map(|result| {
            format!(
                "### {} [{} | {}ms | {}]{}\n{}",
                result.name,
                result.id,
                result.duration_ms,
                if result.is_error { "error" } else { "ok" },
                result
                    .call_summary
                    .as_deref()
                    .map(|summary| format!("\nCall: {summary}"))
                    .unwrap_or_default(),
                crate::truncate(&result.result, 900)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let task_memory_text = task_memory
        .and_then(|selection| memory::format_task_memory_for_prompt(selection, state.intent))
        .unwrap_or_else(|| "none".to_string());

    Some(crate::truncate(
        &format!(
            "Latest user goal:\n{}\n\nRelevant past experience:\n{}\n\nCurrent working state:\n```json\n{}\n```\n\nObservation summaries:\n{}\n\nTool results:\n{}",
            latest_query.unwrap_or("(none)"),
            task_memory_text,
            state_json,
            summary_text,
            result_text
        ),
        6_000,
    ))
}

async fn run_analyze_phase(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
) -> AgentPhaseControl {
    let config = ctx.state.config();
    if phase_state.round >= AGENT_HARD_CAP_ROUNDS {
        let (system_event, mut done_event) = build_agent_hard_cap_events(
            AGENT_HARD_CAP_ROUNDS,
            phase_state.react_ctx.cycles,
            phase_state.react_ctx.tool_calls,
        );
        let usage = build_done_usage(
            ctx.state,
            ctx.current_session_id,
            phase_state.usage_snap_input,
            phase_state.usage_snap_output,
        )
        .await;
        if let (Some(done_obj), Some(usage_obj)) = (done_event.as_object_mut(), usage.as_object()) {
            done_obj.extend(usage_obj.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        if !live_send(ctx.live_tx, system_event).await {
            return AgentPhaseControl::Break;
        }
        let _ = live_send(ctx.live_tx, done_event).await;
        return AgentPhaseControl::Break;
    }

    persist_pending_interventions(
        ctx.state,
        ctx.current_session_id,
        &mut phase_state.pending_interventions,
    )
    .await;
    let snapshot = match prepare_analyze_snapshot(ctx, phase_state).await {
        Some(snapshot) => snapshot,
        None => return AgentPhaseControl::Break,
    };

    let resolved = config.resolve_model(&snapshot.model);
    let auto_signals = runtime_auto_think_signals(phase_state, snapshot.user_msg_chars);
    let auto_decision =
        if snapshot.think_level == "auto" && providers::auto_think_supported(&resolved) {
            Some(agent::auto_think_decision_runtime(auto_signals))
        } else {
            None
        };
    let effective_think = if let Some(decision) = auto_decision.as_ref() {
        decision.selected_level.label().to_owned()
    } else if snapshot.think_level == "auto" {
        "off".to_owned()
    } else {
        snapshot.think_level.clone()
    };
    let extra_tools = build_cycle_tools(ctx, phase_state, &resolved).await;

    let request_budget = crate::context::context_input_budget_for_runtime(
        &config,
        &snapshot.model,
        &effective_think,
    );
    let compression_context = Some(hooks::CompressionContextSections {
        task_state: agent::render_task_state_for_prompt(&phase_state.working_state),
        observation_hint: phase_state.last_observation_hint.clone(),
        task_memory: phase_state
            .retrieved_task_memory
            .as_ref()
            .and_then(|selection| {
                memory::format_task_memory_for_prompt(selection, phase_state.working_state.intent)
            }),
    });

    // Run BeforeAnalyze hooks (including auto-compress) before any destructive
    // request-budget pruning so compression can preserve older history first.
    let before_analyze_events = run_hooks(
        &ctx.state.hooks,
        agent::HookPoint::BeforeAnalyze,
        &ctx.state.sessions,
        ctx.current_session_id,
        &config,
        &ctx.state.http,
        phase_state.react_ctx.cycles,
        compression_context,
        Some(request_budget),
        Some(extra_tools.clone()),
    )
    .await;

    let (extra_pruned_count, request_budget) =
        match fit_messages_to_request_budget(ctx, &snapshot.model, &effective_think, &extra_tools)
            .await
        {
            Some(result) => result,
            None => return AgentPhaseControl::Break,
        };
    let total_pruned_count = snapshot.pruned_count.saturating_add(extra_pruned_count);

    let final_msgs_snapshot =
        match send_before_analyze_events(ctx, before_analyze_events, total_pruned_count).await {
            Some(msgs) => msgs,
            None => return AgentPhaseControl::Break,
        };
    // ── BeforeLlmCall hook (before budget check so estimate includes hook changes) ──
    let llm_hook_input = LlmHookInput {
        messages: final_msgs_snapshot.clone(),
        model: snapshot.model.clone(),
        think_level: effective_think.clone(),
        cycle: phase_state.react_ctx.cycles,
        tool_count: extra_tools.len(),
    };
    let llm_hook_output = run_llm_hooks(&ctx.state.hooks, &llm_hook_input, &config).await;

    let (effective_think, mut final_msgs_snapshot, request_budget) = match llm_hook_output {
        hooks::HookOutput::ModifyLlmParams {
            extra_system,
            think_override,
        } => {
            let has_think_override = think_override.is_some();
            let think = think_override.unwrap_or(effective_think);
            // Recalculate budget when think_level changed so the reserve matches.
            let budget = if has_think_override {
                crate::context::context_input_budget_for_runtime(&config, &snapshot.model, &think)
            } else {
                request_budget
            };
            let msgs = if let Some(extra) = extra_system {
                let mut m = final_msgs_snapshot;
                if let Some(first) = m.first_mut()
                    && first.role == "system"
                    && let Some(content) = first.content.as_mut()
                {
                    content.push('\n');
                    content.push_str(&extra);
                }
                m
            } else {
                final_msgs_snapshot
            };
            (think, msgs, budget)
        }
        _ => (effective_think, final_msgs_snapshot, request_budget),
    };

    // Budget check uses the post-hook snapshot so hook-injected content is accounted for.
    let mut request_estimate = crate::context::estimate_request_tokens_for_provider(
        resolved.provider,
        &final_msgs_snapshot,
        &extra_tools,
    );

    // If hook-modified conditions (think_override / extra_system) made the
    // estimate exceed the (possibly recalculated) budget, re-prune the local
    // snapshot before erroring — the messages may still fit after trimming.
    if request_estimate > request_budget {
        let message_budget = crate::context::request_message_budget_for_runtime(
            &config,
            &snapshot.model,
            &effective_think,
            &extra_tools,
        );
        crate::context::prune_messages_for_provider(
            &mut final_msgs_snapshot,
            resolved.provider,
            message_budget,
        );
        request_estimate = crate::context::estimate_request_tokens_for_provider(
            resolved.provider,
            &final_msgs_snapshot,
            &extra_tools,
        );
    }

    let mut start_event = json!({
        "type":"start",
        "round": phase_state.round + 1,
        "phase": phase_state.react_ctx.phase().label(),
        "cycle": phase_state.react_ctx.cycles,
        "model": snapshot.model.clone(),
        "think_level": effective_think.clone(),
        "react_visible": phase_state.react_ctx.show_react,
        "run_mode": if phase_state.run_mode.is_plan_only() { "plan_only" } else { "execute" },
    });
    if let Some(start_obj) = start_event.as_object_mut()
        && auto_decision.is_some()
    {
        start_obj.insert(
            "auto_observation_strength".to_string(),
            json!(auto_observation_strength_label(
                auto_signals.observation_strength
            )),
        );
        start_obj.insert(
            "auto_stagnation_streak".to_string(),
            json!(auto_signals.stagnation_streak),
        );
        start_obj.insert(
            "auto_error_streak".to_string(),
            json!(auto_signals.error_streak),
        );
        start_obj.insert(
            "auto_task_pressure".to_string(),
            json!(auto_signals.task_pressure),
        );
        start_obj.insert(
            "auto_action_oriented".to_string(),
            json!(auto_signals.action_oriented),
        );
        start_obj.insert(
            "auto_ready_to_finish".to_string(),
            json!(auto_signals.ready_to_finish),
        );
        start_obj.insert(
            "auto_has_blocking_uncertainty".to_string(),
            json!(auto_signals.has_blocking_uncertainty),
        );
    }

    if !live_send(ctx.live_tx, start_event).await {
        return AgentPhaseControl::Break;
    }
    if let Some(task_plan) = phase_state.task_plan.as_ref()
        && !live_send(
            ctx.live_tx,
            json!({
                "type": "task_plan",
                "round": phase_state.round + 1,
                "cycle": phase_state.react_ctx.cycles,
                "plan": task_plan,
            }),
        )
        .await
    {
        return AgentPhaseControl::Break;
    }
    if let Some(auto_trace) = auto_decision.clone().map(|decision| {
        decision.into_trace_with_selected_think(
            phase_state.round + 1,
            phase_state.react_ctx.cycles,
            phase_state.react_ctx.phase().label(),
            &snapshot.model,
            resolved.provider.label(),
            &effective_think,
        )
    }) && !live_send(ctx.live_tx, auto_trace.to_live_event()).await
    {
        return AgentPhaseControl::Break;
    }

    if request_estimate > request_budget {
        phase_state.run_failed = true;
        let _ = live_send(
            ctx.live_tx,
            json!({
                "type":"error",
                "content": format!(
                    "Estimated request size {} exceeds runtime input budget {} after accounting for tools and reasoning. Reduce context, disable MCP servers, lower /think, or switch to a model with a larger context window.",
                    format_token_count(request_estimate as u64),
                    format_token_count(request_budget as u64),
                ),
            }),
        )
        .await;
        return AgentPhaseControl::Break;
    }

    // Agent-level retry: retry the entire LLM call once for transient HTTP-level
    // errors (429/5xx/connect/timeout that already exhausted provider-level retries).
    // Stream-phase errors are NOT retried because partial tokens were already sent.
    // NOTE: BeforeLlmCall hooks are intentionally NOT re-run on retry — the retry
    // reuses the same snapshot produced by the single hook pass above, since hooks
    // modify system prompt / think level which shouldn't change between retries of
    // the same logical request.
    let mut agent_llm_attempt = 0u8;
    let llm_result = loop {
        let result = tokio::select! {
            biased;
            _ = ctx.run_cancel.cancelled() => {
                apply_run_cancel_outcome(ctx, phase_state).await;
                return AgentPhaseControl::Break;
            }
            result = providers::call_llm_stream_with_tool_mode(
                &ctx.state.http,
                &resolved,
                &final_msgs_snapshot,
                &phase_state.cycle_workspace,
                config.s3.as_ref(),
                ctx.live_tx,
                &effective_think,
                &extra_tools,
                !phase_state.run_mode.is_plan_only(),
                config.max_llm_retries,
            ) => result,
        };

        match &result {
            Err(e) if agent_llm_attempt == 0 && providers::is_transient_llm_error(e) => {
                agent_llm_attempt += 1;
                let _ = live_send(
                    ctx.live_tx,
                    json!({"type":"system","content":format!("LLM request failed ({e}), retrying...")}),
                )
                .await;
                // Backoff before agent-level retry, respecting cancellation.
                tokio::select! {
                    biased;
                    _ = ctx.run_cancel.cancelled() => {
                        apply_run_cancel_outcome(ctx, phase_state).await;
                        return AgentPhaseControl::Break;
                    }
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                }
                continue;
            }
            _ => break result,
        }
    };

    match llm_result {
        Ok(resp) => {
            let provider_name = config.resolve_provider_name(&snapshot.model);
            apply_llm_response(
                ctx,
                phase_state,
                resolved.provider,
                provider_name,
                snapshot.usage_role,
                request_estimate as u64,
                snapshot.latest_query.as_deref(),
                resp,
            )
            .await;
            AgentPhaseControl::Continue
        }
        Err(error) => {
            phase_state.run_failed = true;
            let _ = live_send(ctx.live_tx, json!({"type":"error","content":error})).await;
            AgentPhaseControl::Break
        }
    }
}

async fn run_act_phase(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
) -> AgentPhaseControl {
    let config = ctx.state.config();
    phase_state.collected_results.clear();
    let tool_calls = std::mem::take(&mut phase_state.pending_tool_calls);

    let mut all_parallelizable = tool_calls.len() > 1;
    if all_parallelizable {
        for tc in &tool_calls {
            if !tools::is_parallelizable_tool_call(
                &tc.function.name,
                &config,
                &phase_state.cycle_workspace,
            ) {
                all_parallelizable = false;
                break;
            }
        }
    }

    if !all_parallelizable {
        // Sequential path: single tool call or any mutating tool in the batch.
        for tc in &tool_calls {
            if ctx.run_cancel.is_cancelled() {
                apply_run_cancel_outcome(ctx, phase_state).await;
                return AgentPhaseControl::Break;
            }

            let (result, eff_args) = match execute_tool_call(ctx, phase_state, tc).await {
                Ok(pair) => pair,
                Err(control) => return control,
            };

            if matches!(
                record_tool_result(ctx, phase_state, tc, result, eff_args.as_deref()).await,
                AgentPhaseControl::Break
            ) {
                return AgentPhaseControl::Break;
            }
        }
    } else {
        // Multiple parallel-safe tool calls: parallel execution with ordered
        // result recording. Built-in read-only tools and read-only MCP tools
        // can run concurrently; mutating tools and delegated `task` runs stay
        // sequential because they share the parent workspace.
        // 1. Run BeforeToolExec hooks sequentially (may reject or modify args).
        struct HookEvalResult {
            effective_args: Option<String>,
            rejected: Option<tools::ToolOutcome>,
            reject_events: Vec<serde_json::Value>,
        }
        let mut hook_results: Vec<HookEvalResult> = Vec::with_capacity(tool_calls.len());
        for tc in &tool_calls {
            let hook_input = ToolHookInput {
                tool_name: tc.function.name.clone(),
                tool_args: serde_json::from_str(&tc.function.arguments)
                    .unwrap_or_else(|_| serde_json::Value::String(tc.function.arguments.clone())),
                tool_id: tc.id.clone(),
                cycle: phase_state.react_ctx.cycles,
                workspace: phase_state.cycle_workspace.clone(),
                outcome_output: None,
                outcome_is_error: None,
                outcome_duration_ms: None,
            };
            let hook_output = run_tool_hooks(
                &ctx.state.hooks,
                agent::HookPoint::BeforeToolExec,
                hook_input,
                &config,
            )
            .await;
            hook_results.push(match hook_output {
                hooks::HookOutput::Reject { reason, events } => HookEvalResult {
                    effective_args: None,
                    rejected: Some(tools::ToolOutcome {
                        output: format!("[rejected by hook] {reason}"),
                        is_error: true,
                        duration_ms: 0,
                        subagent_snapshot: None,
                    }),
                    reject_events: events,
                },
                hooks::HookOutput::ModifyToolArgs { args } => HookEvalResult {
                    effective_args: Some(
                        serde_json::to_string(&args)
                            .unwrap_or_else(|_| tc.function.arguments.clone()),
                    ),
                    rejected: None,
                    reject_events: Vec::new(),
                },
                _ => HookEvalResult {
                    effective_args: Some(tc.function.arguments.clone()),
                    rejected: None,
                    reject_events: Vec::new(),
                },
            });
        }

        // 2. Send tool_call WS events with effective (possibly hook-modified) args,
        //    then send any reject hook events (matching sequential path: tool_call → hook events).
        for (tc, hr) in tool_calls.iter().zip(hook_results.iter()) {
            if ctx.run_cancel.is_cancelled() {
                apply_run_cancel_outcome(ctx, phase_state).await;
                return AgentPhaseControl::Break;
            }
            // For rejected tools, show original args; for others, show effective args.
            let display_args = if hr.rejected.is_some() {
                &tc.function.arguments
            } else {
                hr.effective_args
                    .as_deref()
                    .unwrap_or(&tc.function.arguments)
            };
            let display_args = tools::display_tool_arguments(&tc.function.name, display_args);
            if !live_send(
                ctx.live_tx,
                json!({
                    "type":"tool_call",
                    "id": tc.id,
                    "name": tc.function.name,
                    "arguments": display_args,
                }),
            )
            .await
            {
                return AgentPhaseControl::Break;
            }
            // Send reject hook events after tool_call (matches sequential path order).
            for ev in &hr.reject_events {
                let _ = live_send(ctx.live_tx, ev.clone()).await;
            }
        }

        // 3. Launch non-rejected tool futures concurrently.
        let futures: Vec<_> = tool_calls
            .iter()
            .zip(hook_results.iter())
            .map(|(tc, hr)| {
                if hr.rejected.is_some() {
                    // Rejected by hook — return a no-op future.
                    return futures::future::Either::Left(async {
                        ToolRunState::Completed(tools::ToolOutcome {
                            output: String::new(), // placeholder, replaced below
                            is_error: true,
                            duration_ms: 0,
                            subagent_snapshot: None,
                        })
                    });
                }
                let args = hr
                    .effective_args
                    .as_deref()
                    .unwrap_or(&tc.function.arguments);
                futures::future::Either::Right(run_tool_with_feedback(
                    ctx.live_tx,
                    ctx.run_cancel,
                    &tc.id,
                    &tc.function.name,
                    runtime_timeout_for_tool(&tc.function.name, &config),
                    execute_tool_with_live_output(
                        ctx.live_tx,
                        &tc.id,
                        &tc.function.name,
                        args,
                        &config,
                        &ctx.state.http,
                        &phase_state.cycle_workspace,
                        true,
                        Some(crate::LiveOutputReplayCtx {
                            state: Arc::clone(ctx.state),
                            session_id: ctx.current_session_id.to_string(),
                        }),
                    ),
                ))
            })
            .collect();

        let results = futures::future::join_all(futures).await;

        // 4. Record results in order, preserving stable tool IDs.
        //    On abort, still record any already-completed results so the LLM
        //    sees side effects (e.g. files written) that already happened.
        let mut should_break = false;
        for (tc, (run_state, hr)) in tool_calls
            .iter()
            .zip(results.into_iter().zip(hook_results.into_iter()))
        {
            // Use the pre-rejected outcome if the hook rejected this tool.
            // For rejected tools, effective_args is None → AfterToolExec hooks are skipped.
            let (effective_run_state, after_args) = if let Some(outcome) = hr.rejected {
                (ToolRunState::Completed(outcome), None)
            } else {
                (run_state, hr.effective_args)
            };
            match effective_run_state {
                ToolRunState::Completed(result) => {
                    if matches!(
                        record_tool_result(ctx, phase_state, tc, result, after_args.as_deref())
                            .await,
                        AgentPhaseControl::Break
                    ) {
                        should_break = true;
                    }
                }
                ToolRunState::Abort => {
                    apply_run_cancel_outcome(ctx, phase_state).await;
                    should_break = true;
                }
            }
        }
        if should_break {
            return AgentPhaseControl::Break;
        }
    }

    finish_act_phase(ctx.live_tx, phase_state, tool_calls.len()).await;
    AgentPhaseControl::Continue
}

async fn run_observe_phase(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
) -> AgentPhaseControl {
    let config = ctx.state.config();
    let summaries = agent::summarize_observations(&phase_state.collected_results);
    let state_before_observe = phase_state.working_state.clone();
    for summary in &summaries {
        let _ = live_send(
            ctx.live_tx,
            json!({
                "type": "observation",
                "tool_call_id": summary.tool_call_id,
                "tool_name": summary.tool_name,
                "byte_size": summary.byte_size,
                "line_count": summary.line_count,
                "hint": summary.hint,
            }),
        )
        .await;
    }
    let consecutive_errors = phase_state
        .collected_results
        .iter()
        .rev()
        .take_while(|r| r.is_error)
        .count();
    let tool_results_count = phase_state.collected_results.len();
    let tool_error_count = phase_state
        .collected_results
        .iter()
        .filter(|result| result.is_error)
        .count();
    let summary_count = summaries.len();
    let summary_bytes = summaries
        .iter()
        .map(|summary| summary.byte_size)
        .sum::<usize>();
    update_working_state(ctx, phase_state, &summaries).await;
    let progress_made =
        agent::auto_think_progress_made(&state_before_observe, &phase_state.working_state);
    let evidence_delta_quality = agent::auto_evidence_delta_quality(
        &state_before_observe,
        &phase_state.working_state,
        progress_made,
    );
    phase_state.last_observation_strength =
        agent::auto_observation_strength(&phase_state.collected_results, &summaries);
    phase_state.last_tool_results_count = tool_results_count;
    phase_state.last_tool_error_count = tool_error_count;
    phase_state.last_summary_count = summary_count;
    phase_state.last_summary_bytes = summary_bytes;
    phase_state.last_progress_made = progress_made;
    phase_state.last_error_kind = agent::auto_error_kind(&phase_state.collected_results);
    phase_state.last_evidence_delta_quality = evidence_delta_quality;
    phase_state.error_streak = if consecutive_errors == 0 {
        0
    } else if progress_made {
        consecutive_errors
    } else {
        phase_state.error_streak.saturating_add(consecutive_errors)
    };
    let should_count_stagnation = !phase_state.collected_results.is_empty()
        || phase_state.working_state.has_blocking_uncertainty();
    if progress_made {
        phase_state.stagnation_streak = 0;
    } else if should_count_stagnation {
        phase_state.stagnation_streak = phase_state.stagnation_streak.saturating_add(1);
    }
    if progress_made {
        phase_state.recent_tool_history.clear();
    }
    phase_state.last_observation_hint =
        agent::build_observation_context_hint(&summaries, consecutive_errors);
    phase_state.collected_results.clear();
    phase_state.results_origin_query = None;

    // Debounced incremental save: skip if saved recently, finish phase always saves.
    let should_save = phase_state
        .last_save_instant
        .map(|t| t.elapsed() >= OBSERVE_SAVE_DEBOUNCE)
        .unwrap_or(true);
    if should_save {
        if let Err(e) =
            session_store::save_current_session_to_disk(ctx.state, ctx.current_session_id).await
        {
            eprintln!("Warning: failed to save session after observe phase: {e}");
        } else {
            phase_state.last_save_instant = Some(std::time::Instant::now());
        }
    }

    let after_observe_events = run_hooks(
        &ctx.state.hooks,
        agent::HookPoint::AfterObserve,
        &ctx.state.sessions,
        ctx.current_session_id,
        &config,
        &ctx.state.http,
        phase_state.react_ctx.cycles,
        None,
        None,
        None,
    )
    .await;

    for event in after_observe_events {
        let _ = live_send(ctx.live_tx, event).await;
    }

    phase_state.react_ctx.transition_to_analyze();
    send_react_phase_event(ctx.live_tx, &phase_state.react_ctx, "analyze").await;
    AgentPhaseControl::Continue
}

async fn register_pending_plan(
    ctx: &AgentRunCtx<'_>,
    phase_state: &AgentPhaseState,
) -> Option<serde_json::Value> {
    if phase_state.react_ctx.finish_reason != Some(agent::FinishReason::Complete) {
        return None;
    }

    let mut sessions = ctx.state.sessions.lock().await;
    let session = sessions.get_mut(ctx.current_session_id)?;
    let original_user_message_index = session
        .messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, message)| message.role == "user")
        .map(|(index, _)| index)?;
    let assistant_plan_message_index = session
        .messages
        .iter()
        .enumerate()
        .rev()
        .find(|(index, message)| {
            *index > original_user_message_index
                && message.role == "assistant"
                && message.has_nonempty_content()
        })
        .map(|(index, _)| index)?;
    let created_at = now_epoch();
    let plan_id = format!(
        "plan_{}_{}_{}",
        created_at, original_user_message_index, assistant_plan_message_index
    );
    session.pending_plan = Some(crate::PendingPlan {
        id: plan_id.clone(),
        original_user_message_index,
        assistant_plan_message_index,
        created_at,
    });
    session.updated_at = created_at;
    Some(json!({
        "type": "plan_ready",
        "plan_id": plan_id,
        "message_index": assistant_plan_message_index,
        "created_at": created_at,
    }))
}

async fn run_finish_phase(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
) -> AgentPhaseControl {
    let config = ctx.state.config();
    let plan_ready_event = if phase_state.run_mode.is_plan_only() {
        register_pending_plan(ctx, phase_state).await
    } else {
        None
    };

    if let Err(e) =
        session_store::save_current_session_to_disk(ctx.state, ctx.current_session_id).await
    {
        eprintln!("Warning: failed to save session at finish phase: {e}");
    }

    let snapshot = {
        let sessions = ctx.state.sessions.lock().await;
        sessions.get(ctx.current_session_id).cloned()
    };

    let on_finish_events = run_hooks(
        &ctx.state.hooks,
        agent::HookPoint::OnFinish,
        &ctx.state.sessions,
        ctx.current_session_id,
        &config,
        &ctx.state.http,
        phase_state.react_ctx.cycles,
        None,
        None,
        None,
    )
    .await;

    for event in on_finish_events {
        let _ = live_send(ctx.live_tx, event).await;
    }

    if let Some(event) = plan_ready_event {
        let _ = live_send(ctx.live_tx, event).await;
    }

    // Enqueue structured memory update (async, non-blocking).
    // Pre-filter messages to avoid cloning the full session history.
    let memory_queue = ctx.state.memory_queue();
    if !phase_state.run_mode.is_plan_only()
        && config.structured_memory
        && let (Some(queue), Some(session)) = (memory_queue.as_ref(), &snapshot)
    {
        let fallback_model = session.effective_model(&config.model);
        let model = config.memory_model_or(fallback_model).to_string();
        let excerpt = crate::memory::prefilter_for_memory(&session.messages);
        queue.enqueue(
            session.id.clone(),
            session.workspace.clone(),
            model,
            excerpt,
        );
    }

    // Post-execution reflection for non-trivial multi-step tasks.
    // Gated by config.daily_reflection + minimum complexity + cooldown.
    // Spawned as a background task to avoid delaying the "done" event.
    // NOTE: snapshot check must precede try_claim_reflection() because the
    // CAS has a side-effect; if it fires but the session is gone, nobody
    // would roll back the cooldown slot.
    if !phase_state.run_mode.is_plan_only()
        && reflection_runtime_enabled()
        && let Some(ref session) = snapshot
        && let Some((previous_epoch, claimed_epoch)) = try_claim_reflection(
            phase_state.react_ctx.cycles,
            phase_state.react_ctx.tool_calls,
        )
    {
        let reflection_generation = reflection_runtime_generation();
        if !reflection_runtime_matches(reflection_generation) {
            rollback_reflection_claim(previous_epoch, claimed_epoch);
        } else {
            let config = ctx.state.config();
            let http = ctx.state.http.clone();
            let sessions = ctx.state.sessions.clone();
            let session_id = session.id.clone();
            let workspace = session.workspace.clone();
            let fallback_model = session.effective_model(&config.model).to_string();
            let model = config.reflection_model_or(&fallback_model).to_string();
            let messages = crate::memory::prefilter_for_memory(&session.messages);
            let cycles = phase_state.react_ctx.cycles;
            let tool_calls = phase_state.react_ctx.tool_calls;
            // Match structured memory: floor at 30s so a low toolTimeout doesn't
            // cause reflections to time out systematically.
            let reflection_timeout = config.tool_timeout.max(std::time::Duration::from_secs(30));
            let reflection_cancel = CancellationToken::new();
            let reflection_task_id = register_active_reflection(reflection_cancel.clone());
            tokio::spawn(async move {
                let outcome = tokio::select! {
                    _ = reflection_cancel.cancelled() => None,
                    outcome = tokio::time::timeout(
                        reflection_timeout,
                        run_post_execution_reflection(PostExecutionReflectionInput {
                            config,
                            http,
                            sessions,
                            session_id,
                            workspace,
                            model,
                            messages,
                            policy_generation: reflection_generation,
                            cycles,
                            tool_calls,
                        }),
                    ) => Some(outcome),
                };
                finish_active_reflection(reflection_task_id);

                match outcome {
                    None => {
                        rollback_reflection_claim(previous_epoch, claimed_epoch);
                    }
                    Some(Ok(Err(e))) => {
                        eprintln!("Reflection failed (non-critical): {e}");
                        // Roll back so the next non-trivial run can try again.
                        rollback_reflection_claim(previous_epoch, claimed_epoch);
                    }
                    Some(Err(_elapsed)) => {
                        eprintln!("Reflection timed out (non-critical)");
                        rollback_reflection_claim(previous_epoch, claimed_epoch);
                    }
                    Some(Ok(Ok(true))) => {
                        // CAS already claimed the slot — nothing more to do.
                    }
                    Some(Ok(Ok(false))) => {
                        // Conversation was too trivial — no reflection written.
                        // Roll back so the next non-trivial run can reflect.
                        rollback_reflection_claim(previous_epoch, claimed_epoch);
                    }
                }
            });
        }
    }

    let finish_label = phase_state
        .react_ctx
        .finish_reason
        .map(|reason| reason.label())
        .unwrap_or("complete");

    let usage = build_done_usage(
        ctx.state,
        ctx.current_session_id,
        phase_state.usage_snap_input,
        phase_state.usage_snap_output,
    )
    .await;

    let mut done_event = json!({
        "type":"done",
        "phase":"finish",
        "reason": finish_label,
        "cycles": phase_state.react_ctx.cycles,
        "tool_calls": phase_state.react_ctx.tool_calls,
    });
    if let (Some(done_obj), Some(usage_obj)) = (done_event.as_object_mut(), usage.as_object()) {
        done_obj.extend(usage_obj.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    let _ = live_send(ctx.live_tx, done_event).await;
    AgentPhaseControl::Break
}

/// Fire `/stop` OnCommand hook in a background task so a slow hook cannot
/// block the stop path.  Best-effort: errors are silently dropped.
fn fire_stop_command_hook(state: &Arc<AppState>, session_id: &str, live_tx: &LiveTx) {
    let state = Arc::clone(state);
    let live_tx = live_tx.clone();
    let session_id = session_id.to_string();
    tokio::spawn(async move {
        let config = state.config();
        let hook_input = CommandHookInput {
            command: "/stop".to_string(),
            args: String::new(),
            result_type: "system".to_string(),
            session_id,
        };
        let hook_events = run_command_hooks(&state.hooks, &hook_input, &config).await;
        for ev in hook_events {
            let _ = live_send(&live_tx, ev).await;
        }
    });
}

async fn apply_run_cancel_outcome(ctx: &AgentRunCtx<'_>, phase_state: &mut AgentPhaseState) {
    let shared_stop_requested = {
        let runs = ctx.state.active_runs.lock().await;
        runs.get(ctx.current_session_id)
            .map(|run| run.stop_requested.swap(false, Ordering::Relaxed))
            .unwrap_or(false)
    };

    if shared_stop_requested {
        fire_stop_command_hook(ctx.state, ctx.current_session_id, ctx.live_tx);
        phase_state.run_stopped = true;
    } else if ctx.cancel.is_cancelled() {
        phase_state.shutting_down = true;
    } else {
        phase_state.run_detached = true;
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_agent_session(
    state: &Arc<AppState>,
    current_session_id: &str,
    connection_id: u64,
    cancel: &CancellationToken,
    live_tx: &LiveTx,
    inbound_rx: &mut mpsc::Receiver<String>,
    stop_requested: &Arc<AtomicBool>,
    run_mode: AgentRunMode,
    reservation: Option<AgentRunReservation>,
) -> AgentRunOutcome {
    let show_react = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(current_session_id)
            .map(|s| s.show_react)
            .unwrap_or(false)
    };

    let reservation = match reservation {
        Some(reservation) => reservation,
        None => match try_reserve_agent_run(
            state,
            current_session_id,
            connection_id,
            cancel,
            stop_requested,
        )
        .await
        {
            Some(reservation) => reservation,
            None => {
                let _ = live_send(
                    live_tx,
                    json!({
                        "type":"system",
                        "content":"Session already has an active run.",
                        "dismissible": true,
                    }),
                )
                .await;
                return AgentRunOutcome {
                    rerun_agent: false,
                    shutting_down: false,
                    run_stopped: false,
                    run_failed: false,
                };
            }
        },
    };
    let run_cancel = reservation.run_cancel;
    let deferred_interventions = reservation.deferred_interventions;

    let ctx = AgentRunCtx {
        state,
        current_session_id,
        cancel,
        live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        results_origin_query: None,
        working_state: agent::WorkingState::default(),
        run_mode,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        last_observation_hint: None,
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(show_react),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
    };

    // Snapshot token counts at loop start so we can compute per-round delta.
    {
        let sessions = state.sessions.lock().await;
        if let Some(s) = sessions.get(current_session_id) {
            phase_state.usage_snap_input = s.input_tokens;
            phase_state.usage_snap_output = s.output_tokens;
        }
    }

    'agent: loop {
        socket_input::drain_shared_interventions(
            &deferred_interventions,
            &mut phase_state.pending_interventions,
        )
        .await;
        if stop_requested.swap(false, Ordering::Relaxed) {
            // Cancel first so running tools/LLM see cancellation immediately.
            run_cancel.cancel();
            // Fire OnCommand hook in background — must not block the stop path.
            fire_stop_command_hook(state, current_session_id, live_tx);
            phase_state.run_stopped = true;
            break;
        }
        if cancel.is_cancelled() {
            phase_state.shutting_down = true;
            break;
        } else if drain_busy_socket_messages(
            state,
            current_session_id,
            inbound_rx,
            &mut phase_state.pending_interventions,
            live_tx,
            &run_cancel,
        )
        .await
        {
            // /stop during busy — fire OnCommand hook in background.
            fire_stop_command_hook(state, current_session_id, live_tx);
            phase_state.run_stopped = true;
            break;
        }
        if run_cancel.is_cancelled() {
            apply_run_cancel_outcome(&ctx, &mut phase_state).await;
            break;
        }
        let control = match phase_state.react_ctx.phase() {
            agent::AgentPhase::Analyze => run_analyze_phase(&ctx, &mut phase_state).await,
            agent::AgentPhase::Act => run_act_phase(&ctx, &mut phase_state).await,
            agent::AgentPhase::Observe => run_observe_phase(&ctx, &mut phase_state).await,
            agent::AgentPhase::Finish => run_finish_phase(&ctx, &mut phase_state).await,
        };

        if matches!(control, AgentPhaseControl::Break) {
            break 'agent;
        }
    }

    socket_input::close_shared_interventions(
        &deferred_interventions,
        &mut phase_state.pending_interventions,
    )
    .await;

    {
        let mut runs = state.active_runs.lock().await;
        if runs.get(current_session_id).map(|run| run.connection_id) == Some(connection_id) {
            runs.remove(current_session_id);
        }
    }

    let rerun_agent = if !phase_state.run_stopped
        && !phase_state.run_detached
        && !phase_state.shutting_down
        && !phase_state.pending_interventions.is_empty()
    {
        persist_pending_interventions(
            state,
            current_session_id,
            &mut phase_state.pending_interventions,
        )
        .await;
        true
    } else {
        false
    };

    if phase_state.run_stopped {
        {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                session_store::trim_incomplete_tool_calls_in_session(session);
            }
        }
        persist_pending_interventions(
            state,
            current_session_id,
            &mut phase_state.pending_interventions,
        )
        .await;
        let usage = build_done_usage(
            state,
            current_session_id,
            phase_state.usage_snap_input,
            phase_state.usage_snap_output,
        )
        .await;
        let mut done_event = json!({
            "type":"done",
            "phase":"stopped",
            "reason":"user_stop",
            "cycles":phase_state.react_ctx.cycles,
            "tool_calls":phase_state.react_ctx.tool_calls
        });
        if let (Some(done_obj), Some(usage_obj)) = (done_event.as_object_mut(), usage.as_object()) {
            done_obj.extend(usage_obj.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        let _ = live_send(live_tx, done_event).await;
    }

    if phase_state.run_detached {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(current_session_id) {
            session_store::trim_incomplete_tool_calls_in_session(session);
        }
    }

    if phase_state.shutting_down {
        let _ = live_send(
            live_tx,
            json!({"type":"system","content":"Server shutting down."}),
        )
        .await;
    }

    AgentRunOutcome {
        rerun_agent,
        shutting_down: phase_state.shutting_down,
        run_stopped: phase_state.run_stopped,
        run_failed: phase_state.run_failed,
    }
}

#[cfg(test)]
#[path = "tests/runtime_loop_tests.rs"]
mod runtime_loop_tests;
