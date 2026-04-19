use super::*;

use serde_json::json;
use std::sync::atomic::{AtomicI64, AtomicU64};
use tokio::time::MissedTickBehavior;

mod socket_input;

pub(crate) use socket_input::{
    IdleSocketInputAction, handle_idle_socket_input, resolve_or_create_socket_session,
};
use socket_input::{drain_busy_socket_messages, persist_pending_interventions};

/// Minimum reasoning cycles before a reflection is worthwhile.
const REFLECTION_MIN_CYCLES: usize = 3;

/// Minimum cooldown between consecutive reflections (seconds).
const REFLECTION_COOLDOWN_SECS: i64 = 600; // 10 minutes

/// Epoch-seconds timestamp of the last reflection run (0 = never).
static LAST_REFLECTION_EPOCH: AtomicI64 = AtomicI64::new(0);

/// Monotonic counter used to make fallback task ids unique even if the system
/// clock has coarse granularity or multiple tasks start within the same tick.
static NEXT_FALLBACK_TASK_ID: AtomicU64 = AtomicU64::new(1);

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

pub(crate) struct AgentRunOutcome {
    pub(crate) rerun_agent: bool,
    pub(crate) shutting_down: bool,
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
    cycle_workspace: PathBuf,
    last_observation_hint: Option<String>,
    pending_interventions: Vec<String>,
    react_ctx: agent::AgentLoopCtx,
    shutting_down: bool,
    run_stopped: bool,
    run_detached: bool,
    last_save_instant: Option<std::time::Instant>,
    /// Token counters snapshotted at loop start for per-round delta calculation.
    usage_snap_input: u64,
    usage_snap_output: u64,
}

/// Minimum interval between observe-phase incremental saves.
const OBSERVE_SAVE_DEBOUNCE: Duration = Duration::from_secs(5);

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
}

enum ToolRunState {
    Completed(tools::ToolOutcome),
    Abort,
}

/// Drop guard that sends a `task_failed` event when a `task` tool future is
/// dropped after `task_started` was emitted but before the terminal event fired
/// (e.g. on timeout or cancellation). Uses `try_send` (non-async, best-effort).
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
            let _ = self.live_tx.try_send(json!({
                "type": "task_failed",
                "task_id": self.task_id,
                "agent": self.agent_name,
                "error": "task aborted (timeout or cancellation)",
            }));
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
        cycles,
        tool_calls,
    } = input;

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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(excerpt),
            images: None,
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

async fn prepare_analyze_snapshot(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
) -> Option<AnalyzeSnapshot> {
    let config = ctx.state.config();
    let mut sessions = ctx.state.sessions.lock().await;
    let session = sessions.get_mut(ctx.current_session_id)?;
    let base_model = session.effective_model(&config.model).to_string();
    let disabled = session.disabled_system_skills.clone();

    // Extract latest user message for query-aware memory retrieval and complexity sensing.
    let latest_query: Option<String> = session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.clone());
    let user_msg_chars = latest_query
        .as_ref()
        .map(|q| q.chars().count())
        .unwrap_or(0);

    // On the first cycle, downgrade to fast model for simple queries when configured.
    let (model_str, usage_role) =
        if phase_state.react_ctx.cycles == 0 && session.model_override.is_none() {
            if let Some(ref fast) = config.fast_model {
                if latest_query
                    .as_deref()
                    .map(agent::is_simple_query)
                    .unwrap_or(false)
                {
                    (fast.clone(), crate::context::USAGE_ROLE_FAST)
                } else {
                    (base_model, crate::context::USAGE_ROLE_PRIMARY)
                }
            } else {
                (base_model, crate::context::USAGE_ROLE_PRIMARY)
            }
        } else {
            (base_model, crate::context::USAGE_ROLE_PRIMARY)
        };

    let mut fresh_system = build_system_prompt_with_query(
        &config,
        &session.workspace,
        &model_str,
        &disabled,
        latest_query.as_deref(),
    );

    // Dynamic context injections into the system prompt:
    // - Observation hint from previous cycle
    // - Planning nudge on first cycle for multi-step tasks
    // - Finish nudge for deep loops
    if let Some(ref mut content) = fresh_system.content {
        if let Some(hint) = phase_state.last_observation_hint.take() {
            content.push_str("\n\n");
            content.push_str(&hint);
        }
        if phase_state.react_ctx.cycles == 0 {
            content.push_str(
                "\n\n## Working Method\n\
                 For complex multi-step tasks, use the `think` tool first to outline your plan \
                 before executing other tools. For simple questions or single-step tasks, respond directly.",
            );
        }
        if let Some(nudge) = agent::build_finish_nudge(phase_state.react_ctx.cycles) {
            content.push_str("\n\n");
            content.push_str(nudge);
        }
    }

    if let Some(first) = session.messages.first_mut()
        && first.role == "system"
    {
        *first = fresh_system;
    }

    let msg_count_before = session.messages.len();
    crate::context::prune_messages_for_provider(
        &mut session.messages,
        config.resolve_model(&model_str).provider,
        context_input_budget_for_model(&config, &model_str),
    );
    let pruned_count = msg_count_before - session.messages.len();

    phase_state.cycle_workspace = session.workspace.clone();

    Some(AnalyzeSnapshot {
        model: model_str,
        usage_role,
        think_level: session.think_level.clone(),
        pruned_count,
        user_msg_chars,
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
    model: &str,
    mut hook_events: Vec<serde_json::Value>,
    pruned_count: usize,
) -> Option<Vec<ChatMessage>> {
    let config = ctx.state.config();
    let final_messages = {
        let sessions = ctx.state.sessions.lock().await;
        sessions
            .get(ctx.current_session_id)
            .map(|session| session.messages.clone())
            .unwrap_or_default()
    };
    let final_context_estimate =
        estimate_tokens_for_provider(config.resolve_model(model).provider, &final_messages);
    for event in &mut hook_events {
        if event["type"] == "context_compressed" {
            event["after_estimate"] = json!(final_context_estimate);
        }
    }

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

fn effective_think_level(
    think_level: &str,
    resolved: &providers::ResolvedModel,
    cycles: usize,
    had_observation_hint: bool,
    user_msg_chars: usize,
    consecutive_errors: usize,
) -> String {
    if think_level == "auto" {
        if resolved.reasoning || resolved.thinking_format.is_some() {
            agent::auto_think_level(
                cycles,
                had_observation_hint,
                user_msg_chars,
                consecutive_errors,
            )
            .to_owned()
        } else {
            "off".to_owned()
        }
    } else {
        think_level.to_owned()
    }
}

async fn build_cycle_tools(
    ctx: &AgentRunCtx<'_>,
    phase_state: &AgentPhaseState,
    resolved: &providers::ResolvedModel,
) -> Vec<serde_json::Value> {
    let config = ctx.state.config();
    build_runtime_tools(&config, resolved.provider, &phase_state.cycle_workspace).await
}

pub(crate) async fn build_runtime_tools(
    config: &Config,
    provider: Provider,
    workspace: &Path,
) -> Vec<serde_json::Value> {
    let mut extra_tools = Vec::new();

    // Sub-agent task + orchestrate tools (only added when agents are discovered)
    let agents = crate::subagents::discovery::discover_all_agents(workspace);
    if !agents.is_empty() {
        let agent_names: Vec<String> = agents.iter().map(|a| a.name.clone()).collect();
        let task_def = match provider {
            Provider::Anthropic => tools::task_tool_definition_anthropic(&agent_names),
            Provider::OpenAI => tools::task_tool_definition_openai(&agent_names),
            Provider::Ollama => tools::task_tool_definition_ollama(&agent_names),
        };
        extra_tools.push(task_def);

        let orchestrate_def = match provider {
            Provider::Anthropic => tools::orchestrate_tool_definition_anthropic(&agent_names),
            Provider::OpenAI => tools::orchestrate_tool_definition_openai(&agent_names),
            Provider::Ollama => tools::orchestrate_tool_definition_ollama(&agent_names),
        };
        extra_tools.push(orchestrate_def);
    }

    let mut mcp_tools = match provider {
        Provider::Anthropic => tools::mcp::tool_definitions_anthropic(config, workspace).await,
        Provider::OpenAI => tools::mcp::tool_definitions_openai(config, workspace).await,
        Provider::Ollama => tools::mcp::tool_definitions_ollama(config, workspace).await,
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

    let mut sessions = ctx.state.sessions.lock().await;
    if let Some(session) = sessions.get_mut(ctx.current_session_id) {
        session.messages.push(message.clone());
        session.updated_at = now_epoch();
    }
}

async fn advance_after_llm_response(
    live_tx: &LiveTx,
    phase_state: &mut AgentPhaseState,
    message: &ChatMessage,
) {
    let has_content = message.has_nonempty_content();
    let has_tools = message.has_tool_calls();

    if let Some(reason) = agent::evaluate_finish(has_content, has_tools) {
        phase_state.react_ctx.transition_to_finish(reason);
        send_react_phase_event(live_tx, &phase_state.react_ctx, "finish").await;
    } else {
        phase_state.pending_tool_calls = message.tool_calls.clone().unwrap_or_default();
        phase_state.react_ctx.transition_to_act();
        send_react_phase_event(live_tx, &phase_state.react_ctx, "act").await;
    }
    phase_state.round += 1;
}

async fn apply_llm_response(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
    resolved_provider: Provider,
    provider_name: String,
    usage_role: &'static str,
    request_input_estimate: u64,
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
    advance_after_llm_response(ctx.live_tx, phase_state, &resp.message).await;
}

async fn execute_tool(
    name: &str,
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
) -> tools::ToolOutcome {
    if let Some(result) = tools::mcp::execute_tool(name, args_str, config, workspace).await {
        result
    } else {
        tools::execute_tool(name, args_str, config, http, workspace).await
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
            };
        }
    };

    // Validate task tool parameters against schema
    if let Some(err) = tools::validate_tool_args("task", &args, &tools::task_tool_parameters()) {
        return tools::ToolOutcome {
            output: err,
            is_error: true,
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    let agent_name = match args.get("agent").and_then(|v| v.as_str()) {
        Some(name) => name,
        None => {
            return tools::ToolOutcome {
                output: "task error: missing required parameter 'agent'".to_string(),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
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
            };
        }
    };

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
        &spec, prompt, config, http, workspace, live_tx, cancel, hooks, &task_id,
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

    tools::ToolOutcome {
        output: outcome.result,
        is_error: outcome.aborted,
        duration_ms,
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
            };
        }
    };

    let outcome = crate::subagents::orchestrator::execute_orchestration(
        &plan, config, http, workspace, live_tx, cancel, hooks,
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

/// Returns `(outcome, effective_args)` where `effective_args` is `None` when
/// the tool was rejected by a BeforeToolExec hook (signals record_tool_result
/// to skip AfterToolExec), or `Some(args_json)` with the actually-executed args.
async fn execute_tool_call(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
    tc: &ToolCall,
) -> Result<(tools::ToolOutcome, Option<String>), AgentPhaseControl> {
    let config = ctx.state.config();
    let tool_timeout = config.tool_timeout;

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
            let _ = live_send(
                ctx.live_tx,
                json!({
                    "type":"tool_call",
                    "id": tc.id,
                    "name": tc.function.name,
                    "arguments": tc.function.arguments,
                }),
            )
            .await;
            for ev in events {
                let _ = live_send(ctx.live_tx, ev).await;
            }
            return Ok((
                tools::ToolOutcome {
                    output: format!("[rejected by hook] {reason}"),
                    is_error: true,
                    duration_ms: 0,
                },
                None, // rejected — skip AfterToolExec
            ));
        }
        hooks::HookOutput::ModifyToolArgs { args } => {
            serde_json::to_string(&args).unwrap_or_else(|_| tc.function.arguments.clone())
        }
        _ => tc.function.arguments.clone(),
    };

    // Send tool_call event with the effective (possibly hook-modified) arguments.
    if !live_send(
        ctx.live_tx,
        json!({
            "type":"tool_call",
            "id": tc.id,
            "name": tc.function.name,
            "arguments": effective_args,
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
    } else {
        run_tool_with_feedback(
            ctx.live_tx,
            ctx.run_cancel,
            &tc.id,
            &tc.function.name,
            Some(tool_timeout),
            execute_tool(
                &tc.function.name,
                &effective_args,
                &config,
                &ctx.state.http,
                &phase_state.cycle_workspace,
            ),
        )
        .await
    };

    match run_state {
        ToolRunState::Completed(result) => Ok((result, Some(effective_args))),
        ToolRunState::Abort => {
            phase_state.shutting_down = ctx.cancel.is_cancelled();
            phase_state.run_detached = !phase_state.shutting_down;
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

    if !live_send(
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

    phase_state.collected_results.push(agent::ToolResultEntry {
        id: tc.id.clone(),
        name: tc.function.name.clone(),
        duration_ms: result.duration_ms,
        is_error: result.is_error,
        result: result.output.clone(),
    });

    {
        let mut sessions = ctx.state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(ctx.current_session_id) {
            if result.is_error {
                session.failed_tool_results.insert(tc.id.clone());
            } else {
                session.failed_tool_results.remove(&tc.id);
            }
            session.messages.push(ChatMessage {
                role: "tool".into(),
                content: Some(result.output),
                images: None,
                tool_calls: None,
                tool_call_id: Some(tc.id.clone()),
                timestamp: Some(now_epoch()),
            });
            session.tool_calls_count += 1;
        }
    }

    AgentPhaseControl::Continue
}

async fn finish_act_phase(live_tx: &LiveTx, phase_state: &mut AgentPhaseState, tc_count: usize) {
    phase_state.react_ctx.transition_to_observe(tc_count);
    send_react_phase_event(live_tx, &phase_state.react_ctx, "observe").await;
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

    let had_observation_hint = phase_state.last_observation_hint.is_some();
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

    // Complexity signals for adaptive think level.
    let consecutive_errors = phase_state
        .collected_results
        .iter()
        .rev()
        .take_while(|r| r.is_error)
        .count();

    let effective_think = effective_think_level(
        &snapshot.think_level,
        &resolved,
        phase_state.react_ctx.cycles,
        had_observation_hint,
        snapshot.user_msg_chars,
        consecutive_errors,
    );
    let extra_tools = build_cycle_tools(ctx, phase_state, &resolved).await;

    // Run BeforeAnalyze hooks (including auto-compress) BEFORE the fine prune
    // so compression can preserve context that would otherwise be hard-deleted.
    let before_analyze_events = run_hooks(
        &ctx.state.hooks,
        agent::HookPoint::BeforeAnalyze,
        &ctx.state.sessions,
        ctx.current_session_id,
        &config,
        &ctx.state.http,
        phase_state.react_ctx.cycles,
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

    let final_msgs_snapshot = match send_before_analyze_events(
        ctx,
        &snapshot.model,
        before_analyze_events,
        total_pruned_count,
    )
    .await
    {
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

    if !live_send(
        ctx.live_tx,
        json!({
            "type":"start",
            "round": phase_state.round + 1,
            "phase": phase_state.react_ctx.phase().label(),
            "react_visible": phase_state.react_ctx.show_react,
        }),
    )
    .await
    {
        return AgentPhaseControl::Break;
    }

    if request_estimate > request_budget {
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
                phase_state.shutting_down = ctx.cancel.is_cancelled();
                phase_state.run_detached = !phase_state.shutting_down;
                return AgentPhaseControl::Break;
            }
            result = providers::call_llm_stream(
                &ctx.state.http,
                &resolved,
                &final_msgs_snapshot,
                &phase_state.cycle_workspace,
                config.s3.as_ref(),
                ctx.live_tx,
                &effective_think,
                &extra_tools,
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
                        phase_state.shutting_down = ctx.cancel.is_cancelled();
                        phase_state.run_detached = !phase_state.shutting_down;
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
                resp,
            )
            .await;
            AgentPhaseControl::Continue
        }
        Err(error) => {
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

    let all_parallelizable = tool_calls.len() > 1
        && tool_calls
            .iter()
            .all(|tc| tools::is_parallelizable_tool(&tc.function.name));

    if !all_parallelizable {
        // Sequential path: single tool call or any mutating tool in the batch.
        for tc in &tool_calls {
            if ctx.run_cancel.is_cancelled() {
                phase_state.shutting_down = ctx.cancel.is_cancelled();
                phase_state.run_detached = !phase_state.shutting_down;
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
        // Multiple parallel-safe read-only tool calls: parallel execution with
        // ordered result recording. Mutating tools and delegated `task` runs
        // stay sequential because they share the parent workspace.
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
                phase_state.shutting_down = ctx.cancel.is_cancelled();
                phase_state.run_detached = !phase_state.shutting_down;
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
        let tool_timeout = config.tool_timeout;
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
                    Some(tool_timeout),
                    execute_tool(
                        &tc.function.name,
                        args,
                        &config,
                        &ctx.state.http,
                        &phase_state.cycle_workspace,
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
                    phase_state.shutting_down = ctx.cancel.is_cancelled();
                    phase_state.run_detached = !phase_state.shutting_down;
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
    phase_state.last_observation_hint =
        agent::build_observation_context_hint(&summaries, consecutive_errors);
    phase_state.collected_results.clear();

    // Debounced incremental save: skip if saved recently, finish phase always saves.
    let should_save = phase_state
        .last_save_instant
        .map(|t| t.elapsed() >= OBSERVE_SAVE_DEBOUNCE)
        .unwrap_or(true);
    if should_save {
        let snapshot = {
            let sessions = ctx.state.sessions.lock().await;
            sessions.get(ctx.current_session_id).cloned()
        };
        if let Some(ref session) = snapshot {
            if let Err(e) = save_session_to_disk(session).await {
                eprintln!("Warning: failed to save session after observe phase: {e}");
            } else {
                phase_state.last_save_instant = Some(std::time::Instant::now());
            }
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
    )
    .await;

    for event in after_observe_events {
        let _ = live_send(ctx.live_tx, event).await;
    }

    phase_state.react_ctx.transition_to_analyze();
    send_react_phase_event(ctx.live_tx, &phase_state.react_ctx, "analyze").await;
    AgentPhaseControl::Continue
}

async fn run_finish_phase(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
) -> AgentPhaseControl {
    let config = ctx.state.config();
    let snapshot = {
        let sessions = ctx.state.sessions.lock().await;
        sessions.get(ctx.current_session_id).cloned()
    };
    if let Some(ref session) = snapshot
        && let Err(e) = save_session_to_disk(session).await
    {
        eprintln!("Warning: failed to save session at finish phase: {e}");
    }

    let on_finish_events = run_hooks(
        &ctx.state.hooks,
        agent::HookPoint::OnFinish,
        &ctx.state.sessions,
        ctx.current_session_id,
        &config,
        &ctx.state.http,
        phase_state.react_ctx.cycles,
    )
    .await;

    for event in on_finish_events {
        let _ = live_send(ctx.live_tx, event).await;
    }

    // Enqueue structured memory update (async, non-blocking).
    // Pre-filter messages to avoid cloning the full session history.
    if let (Some(queue), Some(session)) = (&ctx.state.memory_queue, &snapshot) {
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
    if config.daily_reflection
        && let Some(ref session) = snapshot
        && let Some((previous_epoch, claimed_epoch)) = try_claim_reflection(
            phase_state.react_ctx.cycles,
            phase_state.react_ctx.tool_calls,
        )
    {
        let config = config.clone();
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
        tokio::spawn(async move {
            match tokio::time::timeout(
                reflection_timeout,
                run_post_execution_reflection(PostExecutionReflectionInput {
                    config,
                    http,
                    sessions,
                    session_id,
                    workspace,
                    model,
                    messages,
                    cycles,
                    tool_calls,
                }),
            )
            .await
            {
                Ok(Err(e)) => {
                    eprintln!("Reflection failed (non-critical): {e}");
                    // Roll back so the next non-trivial run can try again.
                    rollback_reflection_claim(previous_epoch, claimed_epoch);
                }
                Err(_elapsed) => {
                    eprintln!("Reflection timed out (non-critical)");
                    rollback_reflection_claim(previous_epoch, claimed_epoch);
                }
                Ok(Ok(true)) => {
                    // CAS already claimed the slot — nothing more to do.
                }
                Ok(Ok(false)) => {
                    // Conversation was too trivial — no reflection written.
                    // Roll back so the next non-trivial run can reflect.
                    rollback_reflection_claim(previous_epoch, claimed_epoch);
                }
            }
        });
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

pub(crate) async fn run_agent_session(
    state: &Arc<AppState>,
    current_session_id: &str,
    connection_id: u64,
    cancel: &CancellationToken,
    live_tx: &LiveTx,
    inbound_rx: &mut mpsc::Receiver<String>,
    stop_requested: &Arc<AtomicBool>,
) -> AgentRunOutcome {
    let show_react = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(current_session_id)
            .map(|s| s.show_react)
            .unwrap_or(false)
    };

    let run_cancel = cancel.child_token();
    {
        let mut runs = state.active_runs.lock().await;
        runs.insert(
            current_session_id.to_string(),
            SessionRunBinding {
                connection_id,
                cancel: run_cancel.clone(),
            },
        );
    }

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
        cycle_workspace: PathBuf::new(),
        last_observation_hint: None,
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(show_react),
        shutting_down: false,
        run_stopped: false,
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
        if cancel.is_cancelled() {
            phase_state.shutting_down = true;
            break;
        }
        if run_cancel.is_cancelled() {
            phase_state.shutting_down = cancel.is_cancelled();
            phase_state.run_detached = !phase_state.shutting_down;
            break;
        }
        if stop_requested.swap(false, Ordering::Relaxed) {
            // Cancel first so running tools/LLM see cancellation immediately.
            run_cancel.cancel();
            // Fire OnCommand hook in background — must not block the stop path.
            fire_stop_command_hook(state, current_session_id, live_tx);
            phase_state.run_stopped = true;
            break;
        } else if drain_busy_socket_messages(
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
        if run_cancel.is_cancelled() && !cancel.is_cancelled() {
            phase_state.run_stopped = true;
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
        persist_pending_interventions(
            state,
            current_session_id,
            &mut phase_state.pending_interventions,
        )
        .await;
        {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                session_store::trim_incomplete_tool_calls(&mut session.messages);
            }
        }
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
            session_store::trim_incomplete_tool_calls(&mut session.messages);
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
    }
}

#[cfg(test)]
#[path = "tests/runtime_loop_tests.rs"]
mod runtime_loop_tests;
