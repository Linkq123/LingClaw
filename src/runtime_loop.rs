use super::*;

use serde_json::json;
use tokio::time::MissedTickBehavior;

mod socket_input;

use socket_input::{drain_busy_socket_messages, persist_pending_interventions};
pub(crate) use socket_input::{
    handle_idle_socket_input, resolve_or_create_socket_session, IdleSocketInputAction,
};

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
}

enum AgentPhaseControl {
    Continue,
    Break,
}

struct AnalyzeSnapshot {
    model: String,
    think_level: String,
    pruned_count: usize,
}

enum ToolRunState {
    Completed(tools::ToolOutcome),
    Abort,
}

const AGENT_HARD_CAP_ROUNDS: usize = 200;

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
    let mut sessions = ctx.state.sessions.lock().await;
    let session = sessions.get_mut(ctx.current_session_id)?;
    let model_str = session.effective_model(&ctx.state.config.model).to_string();
    let disabled = session.disabled_system_skills.clone();
    let mut fresh_system =
        build_system_prompt(&ctx.state.config, &session.workspace, &model_str, &disabled);
    if let Some(hint) = phase_state.last_observation_hint.take() {
        if let Some(ref mut content) = fresh_system.content {
            content.push_str("\n\n");
            content.push_str(&hint);
        }
    }
    if let Some(first) = session.messages.first_mut() {
        if first.role == "system" {
            *first = fresh_system;
        }
    }

    let msg_count_before = session.messages.len();
    crate::context::prune_messages_for_provider(
        &mut session.messages,
        ctx.state.config.resolve_model(&model_str).provider,
        context_input_budget_for_model(&ctx.state.config, &model_str),
    );
    let pruned_count = msg_count_before - session.messages.len();

    phase_state.cycle_workspace = session.workspace.clone();

    Some(AnalyzeSnapshot {
        model: model_str,
        think_level: session.think_level.clone(),
        pruned_count,
    })
}

async fn fit_messages_to_request_budget(
    ctx: &AgentRunCtx<'_>,
    model: &str,
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> Option<(usize, usize)> {
    let provider = ctx.state.config.resolve_model(model).provider;
    let request_budget =
        crate::context::context_input_budget_for_runtime(&ctx.state.config, model, think_level);
    let message_budget = crate::context::request_message_budget_for_runtime(
        &ctx.state.config,
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
    react_ctx: &agent::AgentLoopCtx,
    model: &str,
    pruned_count: usize,
) -> Option<Vec<ChatMessage>> {
    let mut before_analyze_events = run_hooks(
        &ctx.state.hooks,
        agent::HookPoint::BeforeAnalyze,
        &ctx.state.sessions,
        ctx.current_session_id,
        &ctx.state.config,
        &ctx.state.http,
        react_ctx.cycles,
    )
    .await;

    let final_messages = {
        let sessions = ctx.state.sessions.lock().await;
        sessions
            .get(ctx.current_session_id)
            .map(|session| session.messages.clone())
            .unwrap_or_default()
    };
    let final_context_estimate = estimate_tokens_for_provider(
        ctx.state.config.resolve_model(model).provider,
        &final_messages,
    );
    for event in &mut before_analyze_events {
        if event["type"] == "context_compressed" {
            event["after_estimate"] = json!(final_context_estimate);
        }
    }

    for event in before_analyze_events {
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
) -> String {
    if think_level == "auto" {
        if resolved.reasoning || resolved.thinking_format.is_some() {
            agent::auto_think_level(cycles, had_observation_hint).to_owned()
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
    build_runtime_tools(
        &ctx.state.config,
        resolved.provider,
        &phase_state.cycle_workspace,
    )
    .await
}

pub(crate) async fn build_runtime_tools(
    config: &Config,
    provider: Provider,
    workspace: &Path,
) -> Vec<serde_json::Value> {
    let mut extra_tools = Vec::new();
    let mut mcp_tools = match provider {
        Provider::Anthropic => tools::mcp::tool_definitions_anthropic(config, workspace).await,
        Provider::OpenAI => tools::mcp::tool_definitions_openai(config, workspace).await,
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
    request_input_estimate: u64,
    resp: &providers::LlmResponse,
) {
    let input_tokens = resp.input_tokens.unwrap_or(request_input_estimate);
    let output_tokens = resp
        .output_tokens
        .unwrap_or_else(|| message_token_len_for_provider(resolved_provider, &resp.message) as u64);

    let mut sessions = ctx.state.sessions.lock().await;
    if let Some(session) = sessions.get_mut(ctx.current_session_id) {
        update_session_token_usage(
            session,
            input_tokens,
            output_tokens,
            token_usage_source(resp.input_tokens),
            token_usage_source(resp.output_tokens),
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
    request_input_estimate: u64,
    resp: providers::LlmResponse,
) {
    update_llm_response_usage(ctx, resolved_provider, request_input_estimate, &resp).await;
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

async fn run_tool_with_feedback<F>(
    live_tx: &LiveTx,
    cancel: &CancellationToken,
    tool_id: &str,
    tool_name: &str,
    timeout: Duration,
    future: F,
) -> ToolRunState
where
    F: std::future::Future<Output = tools::ToolOutcome>,
{
    let start = std::time::Instant::now();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(TOOL_PROGRESS_HEARTBEAT_SECS));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    heartbeat.tick().await;

    let timeout_secs = timeout.as_secs();
    let sleep = tokio::time::sleep(timeout);
    tokio::pin!(sleep);
    tokio::pin!(future);

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return ToolRunState::Abort;
            }
            _ = &mut sleep => {
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

async fn execute_tool_call(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
    tc: &ToolCall,
) -> Result<tools::ToolOutcome, AgentPhaseControl> {
    let tool_timeout = ctx.state.config.tool_timeout;

    if !live_send(
        ctx.live_tx,
        json!({
            "type":"tool_call",
            "id": tc.id,
            "name": tc.function.name,
            "arguments": tc.function.arguments,
        }),
    )
    .await
    {
        return Err(AgentPhaseControl::Break);
    }

    let run_state = run_tool_with_feedback(
        ctx.live_tx,
        ctx.run_cancel,
        &tc.id,
        &tc.function.name,
        tool_timeout,
        execute_tool(
            &tc.function.name,
            &tc.function.arguments,
            &ctx.state.config,
            &ctx.state.http,
            &phase_state.cycle_workspace,
        ),
    )
    .await;

    match run_state {
        ToolRunState::Completed(result) => Ok(result),
        ToolRunState::Abort => {
            phase_state.shutting_down = ctx.cancel.is_cancelled();
            phase_state.run_detached = !phase_state.shutting_down;
            Err(AgentPhaseControl::Break)
        }
    }
}

async fn record_tool_result(
    ctx: &AgentRunCtx<'_>,
    phase_state: &mut AgentPhaseState,
    tc: &ToolCall,
    result: tools::ToolOutcome,
) -> AgentPhaseControl {
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
            session.messages.push(ChatMessage {
                role: "tool".into(),
                content: Some(result.output),
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
    if phase_state.round >= AGENT_HARD_CAP_ROUNDS {
        let (system_event, done_event) = build_agent_hard_cap_events(
            AGENT_HARD_CAP_ROUNDS,
            phase_state.react_ctx.cycles,
            phase_state.react_ctx.tool_calls,
        );
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

    let resolved = ctx.state.config.resolve_model(&snapshot.model);
    let effective_think = effective_think_level(
        &snapshot.think_level,
        &resolved,
        phase_state.react_ctx.cycles,
        had_observation_hint,
    );
    let extra_tools = build_cycle_tools(ctx, phase_state, &resolved).await;

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
        &phase_state.react_ctx,
        &snapshot.model,
        total_pruned_count,
    )
    .await
    {
        Some(msgs) => msgs,
        None => return AgentPhaseControl::Break,
    };
    let request_estimate = crate::context::estimate_request_tokens_for_provider(
        resolved.provider,
        &final_msgs_snapshot,
        &extra_tools,
    );

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

    let llm_result = tokio::select! {
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
            ctx.live_tx,
            &effective_think,
            &extra_tools,
        ) => result,
    };

    match llm_result {
        Ok(resp) => {
            apply_llm_response(
                ctx,
                phase_state,
                resolved.provider,
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
    phase_state.collected_results.clear();
    let tool_calls = std::mem::take(&mut phase_state.pending_tool_calls);

    for tc in &tool_calls {
        if ctx.run_cancel.is_cancelled() {
            phase_state.shutting_down = ctx.cancel.is_cancelled();
            phase_state.run_detached = !phase_state.shutting_down;
            return AgentPhaseControl::Break;
        }

        let result = match execute_tool_call(ctx, phase_state, tc).await {
            Ok(result) => result,
            Err(control) => return control,
        };

        if matches!(
            record_tool_result(ctx, phase_state, tc, result).await,
            AgentPhaseControl::Break
        ) {
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
    phase_state.last_observation_hint = agent::build_observation_context_hint(&summaries);
    phase_state.collected_results.clear();

    let snapshot = {
        let sessions = ctx.state.sessions.lock().await;
        sessions.get(ctx.current_session_id).cloned()
    };
    if let Some(ref session) = snapshot {
        let _ = save_session_to_disk(session).await;
    }

    let after_observe_events = run_hooks(
        &ctx.state.hooks,
        agent::HookPoint::AfterObserve,
        &ctx.state.sessions,
        ctx.current_session_id,
        &ctx.state.config,
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
    let snapshot = {
        let sessions = ctx.state.sessions.lock().await;
        sessions.get(ctx.current_session_id).cloned()
    };
    if let Some(ref session) = snapshot {
        let _ = save_session_to_disk(session).await;
    }

    let on_finish_events = run_hooks(
        &ctx.state.hooks,
        agent::HookPoint::OnFinish,
        &ctx.state.sessions,
        ctx.current_session_id,
        &ctx.state.config,
        &ctx.state.http,
        phase_state.react_ctx.cycles,
    )
    .await;

    for event in on_finish_events {
        let _ = live_send(ctx.live_tx, event).await;
    }

    let finish_label = phase_state
        .react_ctx
        .finish_reason
        .map(|reason| reason.label())
        .unwrap_or("complete");

    let _ = live_send(
        ctx.live_tx,
        json!({
            "type":"done",
            "phase":"finish",
            "reason": finish_label,
            "cycles": phase_state.react_ctx.cycles,
            "tool_calls": phase_state.react_ctx.tool_calls,
        }),
    )
    .await;
    AgentPhaseControl::Break
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
    };

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
            run_cancel.cancel();
            phase_state.run_stopped = true;
            break;
        }
        if drain_busy_socket_messages(
            inbound_rx,
            &mut phase_state.pending_interventions,
            live_tx,
            &run_cancel,
        )
        .await
        {
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
        let _ = live_send(
            live_tx,
            json!({
                "type":"done",
                "phase":"stopped",
                "reason":"user_stop",
                "cycles":phase_state.react_ctx.cycles,
                "tool_calls":phase_state.react_ctx.tool_calls
            }),
        )
        .await;
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
