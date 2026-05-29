// ══════════════════════════════════════════════════════════════════════════════
//  Sub-Agent Executor
//
//  Runs a sub-agent in an isolated context: separate message history,
//  filtered tool set, independent ReAct loop. Results are returned as a
//  string to be injected back into the parent agent's Observe phase.
//
//  Inspired by:
//  - DeerFlow: background execution with timeout + event streaming
//  - OpenCode: per-agent model/tool/permission configuration
//  - OpenClaw: session-level isolation
// ══════════════════════════════════════════════════════════════════════════════

use std::{collections::HashMap, path::Path, time::Duration};

use futures::FutureExt;
use reqwest::Client;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::SubAgentSpec;
use crate::{
    ChatMessage, Config, LiveTx, SubagentHistorySnapshot, SubagentToolHistorySnapshot, agent,
    context,
    hooks::{self, HookRegistry, ToolHookInput, run_tool_hooks},
    live_send, prompts, providers, tools, truncate,
};

/// Maximum characters in the sub-agent's final result returned to the parent.
const MAX_RESULT_CHARS: usize = 30_000;
const MAX_SNAPSHOT_REASONING_CHARS: usize = 12_000;
const MAX_SNAPSHOT_TOOL_ARGS_CHARS: usize = 4_000;
const MAX_SNAPSHOT_TOOL_RESULT_CHARS: usize = 8_000;
const MAX_SNAPSHOT_RESULT_CHARS: usize = 4_000;
const DELEGATED_PROMPT_CONTEXT_HEADING: &str = "## Delegated Task Context";

fn runtime_timeout_for_subagent_tool(tool_name: &str, config: &Config) -> Option<Duration> {
    tools::tool_runtime_timeout(tool_name, config)
}

fn append_reasoning_snapshot(snapshot: &mut SubagentHistorySnapshot, cycle: usize, text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let entry = format!("[Cycle {cycle}]\n{trimmed}");
    let reasoning = snapshot.reasoning.get_or_insert_with(String::new);
    if !reasoning.is_empty() {
        reasoning.push_str("\n\n");
    }
    reasoning.push_str(&entry);
    if reasoning.len() > MAX_SNAPSHOT_REASONING_CHARS {
        *reasoning = truncate(reasoning, MAX_SNAPSHOT_REASONING_CHARS).to_string();
    }
}

fn truncated_option(text: &str, limit: usize) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(truncate(trimmed, limit).to_string())
    }
}

pub(crate) struct ParallelToolBatchResult {
    pub results: Vec<Option<tools::ToolOutcome>>,
    pub interrupted: bool,
    pub timed_out: bool,
}

pub(crate) fn augment_subagent_prompt_with_runtime_context(
    prompt: &str,
    local_time: &str,
) -> String {
    if prompt
        .trim_start()
        .starts_with(DELEGATED_PROMPT_CONTEXT_HEADING)
    {
        return prompt.to_string();
    }

    format!(
        "{DELEGATED_PROMPT_CONTEXT_HEADING}\n\
         - Current system local time: {local_time}\n\n\
         ## Delegated Task\n\
         {}",
        prompt.trim()
    )
}

pub(crate) fn augment_subagent_prompt_with_current_time(prompt: &str) -> String {
    let local_time = prompts::current_local_snapshot().datetime_label();
    augment_subagent_prompt_with_runtime_context(prompt, &local_time)
}

pub(crate) async fn collect_parallel_tool_results(
    tool_futures: Vec<
        std::pin::Pin<
            Box<dyn std::future::Future<Output = Option<tools::ToolOutcome>> + Send + 'static>,
        >,
    >,
    cancel: &CancellationToken,
    deadline: Option<tokio::time::Instant>,
) -> ParallelToolBatchResult {
    let mut join_set = tokio::task::JoinSet::new();
    let mut results: Vec<Option<tools::ToolOutcome>> = std::iter::repeat_with(|| None)
        .take(tool_futures.len())
        .collect();

    for (index, future) in tool_futures.into_iter().enumerate() {
        join_set.spawn(async move { (index, future.await) });
    }

    let mut interrupted = false;
    let mut timed_out = false;

    while !join_set.is_empty() {
        let join_result = if let Some(deadline) = deadline {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    interrupted = true;
                    break;
                }
                _ = tokio::time::sleep_until(deadline) => {
                    interrupted = true;
                    timed_out = true;
                    break;
                }
                result = join_set.join_next() => result,
            }
        } else {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    interrupted = true;
                    break;
                }
                result = join_set.join_next() => result,
            }
        };

        let Some(join_result) = join_result else {
            break;
        };
        match join_result {
            Ok((index, outcome)) => {
                results[index] = outcome;
            }
            Err(error) => {
                eprintln!("[subagent-parallel] tool future dropped before completion: {error}");
            }
        }
    }

    if interrupted {
        join_set.abort_all();
        while let Some(join_result) = join_set.join_next().await {
            match join_result {
                Ok((index, outcome)) => {
                    results[index] = outcome;
                }
                Err(error) => {
                    if !error.is_cancelled() {
                        eprintln!("[subagent-parallel] tool future failed while draining: {error}");
                    }
                }
            }
        }
    }

    ParallelToolBatchResult {
        results,
        interrupted,
        timed_out,
    }
}

fn interrupted_parallel_tool_outcome(
    tool_name: &str,
    interrupted: bool,
    timed_out: bool,
    duration_ms: u64,
) -> tools::ToolOutcome {
    let output = if interrupted {
        if timed_out {
            format!("Tool '{}' aborted: sub-agent deadline exceeded", tool_name)
        } else {
            format!("Tool '{}' aborted: sub-agent cancelled", tool_name)
        }
    } else {
        format!(
            "Tool '{}' failed: internal parallel executor did not return a result",
            tool_name
        )
    };

    tools::ToolOutcome {
        output,
        is_error: true,
        duration_ms,
        subagent_snapshot: None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn finalize_parallel_batch_outcome(
    hooks: &HookRegistry,
    config: &Config,
    workspace: &Path,
    cycle: usize,
    tool_name: &str,
    effective_args: &str,
    tool_id: &str,
    result: Option<tools::ToolOutcome>,
    interrupted: bool,
    timed_out: bool,
    duration_ms: u64,
) -> tools::ToolOutcome {
    match result {
        Some(outcome) => {
            apply_after_tool_exec_hook(
                hooks,
                config,
                workspace,
                cycle,
                tool_name,
                effective_args,
                tool_id,
                outcome,
            )
            .await
        }
        None => interrupted_parallel_tool_outcome(tool_name, interrupted, timed_out, duration_ms),
    }
}

/// Apply the sub-agent AfterToolExec hook to a real tool outcome.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_after_tool_exec_hook(
    hooks: &HookRegistry,
    config: &Config,
    workspace: &Path,
    cycle: usize,
    tool_name: &str,
    effective_args: &str,
    tool_id: &str,
    mut outcome: tools::ToolOutcome,
) -> tools::ToolOutcome {
    let after_input = ToolHookInput {
        tool_name: tool_name.to_string(),
        tool_args: serde_json::from_str(effective_args)
            .unwrap_or_else(|_| serde_json::Value::String(effective_args.to_string())),
        tool_id: tool_id.to_string(),
        cycle,
        workspace: workspace.to_path_buf(),
        outcome_output: Some(outcome.output.clone()),
        outcome_is_error: Some(outcome.is_error),
        outcome_duration_ms: Some(outcome.duration_ms),
    };
    let after_output =
        run_tool_hooks(hooks, agent::HookPoint::AfterToolExec, after_input, config).await;
    if let hooks::HookOutput::ModifyToolResult { result } = after_output {
        outcome.output = result;
    }
    outcome
}

async fn emit_subagent_tool_result_event(
    live_tx: &LiveTx,
    task_id: &str,
    agent_name: &str,
    tool_name: &str,
    tool_id: &str,
    outcome: &tools::ToolOutcome,
) {
    let _ = live_send(
        live_tx,
        json!({
            "type": "tool_result",
            "task_id": task_id,
            "subagent": agent_name,
            "id": tool_id,
            "name": tool_name,
            "result": crate::truncate(&outcome.output, 8_000),
            "duration_ms": outcome.duration_ms,
            "is_error": outcome.is_error,
        }),
    )
    .await;
}

async fn emit_subagent_tool_output_event(
    live_tx: &LiveTx,
    task_id: &str,
    agent_name: &str,
    tool_name: &str,
    tool_id: &str,
    stream: &'static str,
    chunk: &str,
    replay_ctx: Option<&crate::LiveOutputReplayCtx>,
) {
    crate::forward_tool_output_event_best_effort(
        live_tx,
        json!({
            "type": "tool_output",
            "task_id": task_id,
            "subagent": agent_name,
            "id": tool_id,
            "name": tool_name,
            "stream": stream,
            "chunk": chunk,
        }),
        replay_ctx,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_subagent_tool_with_live_output(
    live_tx: &LiveTx,
    cancel: &CancellationToken,
    task_id: &str,
    agent_name: &str,
    tool_name: &str,
    tool_id: &str,
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
    _isolated_mcp_session: bool,
    replay_ctx: Option<crate::LiveOutputReplayCtx>,
) -> tools::ToolOutcome {
    let start = tokio::time::Instant::now();
    let (event_tx, mut event_rx) =
        tokio::sync::mpsc::channel(tools::TOOL_LIVE_EVENT_CHANNEL_CAPACITY);
    let outcome = execute_subagent_tool(
        tool_name,
        args_str,
        config,
        http,
        workspace,
        _isolated_mcp_session,
        None,
        Some(event_tx),
    );
    let timeout = runtime_timeout_for_subagent_tool(tool_name, config);
    let has_timeout = timeout.is_some();
    let timeout_secs = timeout.map(|value| value.as_secs()).unwrap_or(0);
    let sleep = tokio::time::sleep(timeout.unwrap_or(Duration::ZERO));
    tokio::pin!(sleep);
    tokio::pin!(outcome);
    let mut forward_event = |stream, chunk: String| {
        let replay_ctx = replay_ctx.clone();
        async move {
            emit_subagent_tool_output_event(
                live_tx,
                task_id,
                agent_name,
                tool_name,
                tool_id,
                stream,
                &chunk,
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
            return outcome.as_mut().await;
        }

        if let Some(result) = outcome.as_mut().now_or_never() {
            pending_result = Some(result);
            continue;
        }

        tokio::select! {
            biased;
            result = &mut outcome => {
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
            _ = cancel.cancelled() => {
                return tools::ToolOutcome {
                    output: format!("Tool '{tool_name}' aborted: sub-agent cancelled"),
                    is_error: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                    subagent_snapshot: None,
                };
            }
            _ = &mut sleep, if has_timeout => {
                return tools::ToolOutcome {
                    output: format!("{tool_name} error: tool execution timed out ({}s)", timeout_secs),
                    is_error: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                    subagent_snapshot: None,
                };
            }
        }
    }
}

/// Sub-agent execution outcome.
pub(crate) struct SubAgentOutcome {
    /// Final text result to inject into parent context.
    pub result: String,
    /// Number of ReAct cycles completed.
    pub cycles: usize,
    /// Number of tool calls executed.
    pub tool_calls: usize,
    /// Whether the execution was aborted (cancel/timeout).
    pub aborted: bool,
    /// Total input tokens consumed across all LLM calls made by this sub-agent.
    /// Uses provider-reported usage when available; falls back to a local
    /// estimate so parent usage tracking still reflects sub-agent cost.
    pub total_input_tokens: u64,
    /// Total output tokens consumed across all LLM calls made by this sub-agent.
    pub total_output_tokens: u64,
    /// Per-provider usage aggregated across the sub-agent run.
    pub provider_usage: HashMap<String, [u64; 2]>,
    /// Compact history snapshot for restoring delegated task cards after reload.
    pub history_snapshot: crate::SubagentHistorySnapshot,
}

/// Resolve which model a sub-agent should use.
/// Fallback chain: `sub-agent-<name>` -> `sub-agent` -> primary.
pub(crate) fn resolve_subagent_model<'a>(config: &'a Config, agent_name: &str) -> &'a str {
    config.sub_agent_model_for(agent_name)
}

fn accumulate_usage(
    total_input_tokens: &mut u64,
    total_output_tokens: &mut u64,
    provider_usage: &mut HashMap<String, [u64; 2]>,
    provider_name: &str,
    input_used: u64,
    output_used: u64,
) {
    *total_input_tokens = total_input_tokens.saturating_add(input_used);
    *total_output_tokens = total_output_tokens.saturating_add(output_used);
    let entry = provider_usage
        .entry(context::usage_provider_label(provider_name))
        .or_insert([0, 0]);
    entry[0] = entry[0].saturating_add(input_used);
    entry[1] = entry[1].saturating_add(output_used);
}

fn last_assistant_message(messages: &[ChatMessage]) -> Option<&ChatMessage> {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant")
}

fn final_assistant_content(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant" && message.has_nonempty_content())
        .and_then(|message| message.content.clone())
        .unwrap_or_default()
}

fn final_tool_content(messages: &[ChatMessage]) -> String {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "tool" && message.has_nonempty_content())
        .and_then(|message| message.content.clone())
        .unwrap_or_default()
}

fn needs_forced_final_response(messages: &[ChatMessage]) -> bool {
    last_assistant_message(messages)
        .is_some_and(|message| message.has_tool_calls() || !message.has_nonempty_content())
}

fn build_forced_final_response_prompt() -> String {
    "The delegated run is ending now. Provide your final response for the parent agent using only the information already gathered. Follow your normal output format. Do not call tools, do not continue investigating, and do not end with a note about checking more files.".to_string()
}

async fn request_forced_final_response(
    model_id: &str,
    resolved: &providers::ResolvedModel,
    config: &Config,
    http: &Client,
    workspace: &Path,
    messages: &[ChatMessage],
) -> Result<(Vec<ChatMessage>, providers::SimpleLlmResponse), String> {
    let mut final_messages = messages.to_vec();
    final_messages.push(ChatMessage {
        role: "user".into(),
        content: Some(build_forced_final_response_prompt()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });

    let budget = context::message_budget_for_tool_defs(config, model_id, "off", &[]);
    context::prune_messages_for_provider(&mut final_messages, resolved.provider, budget);

    let response = providers::call_llm_simple_with_usage(
        http,
        resolved,
        &final_messages,
        workspace,
        config.s3.as_ref(),
        "off",
        config.max_llm_retries,
    )
    .await?;

    Ok((final_messages, response))
}

/// Run a sub-agent with full isolation.
///
/// The sub-agent gets:
/// - Its own message history (system + user prompt)
/// - Filtered tools based on spec.tools
/// - Independent ReAct loop with max_turns limit
/// - Result streamed back via parent's LiveTx with prefixed events
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_subagent(
    spec: &SubAgentSpec,
    prompt: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
    parent_live_tx: &LiveTx,
    cancel: CancellationToken,
    hooks: &HookRegistry,
    replay_ctx: Option<crate::LiveOutputReplayCtx>,
    task_id: &str,
) -> SubAgentOutcome {
    let model_id = resolve_subagent_model(config, &spec.name).to_string();
    let resolved = config.resolve_model(&model_id);
    let provider_name = config.resolve_provider_name(&model_id);

    // Ensure MCP tool cache is warm before building the sub-agent tool set.
    // The main loop's Analyze phase usually warms it, but cache may have expired
    // (TTL=30s) if LLM inference was slow, or if this is a re-invocation.
    tools::mcp::ensure_tools_cached(config, workspace).await;

    // Build filtered tool definitions for this sub-agent (includes MCP tools).
    let allowed_tools = super::filter_tools_for_agent_with_mcp(spec, config, workspace);
    let tool_defs = build_filtered_tool_defs(&allowed_tools, config, workspace, resolved.provider);

    // Build isolated message history.
    let system_prompt = build_subagent_system_prompt(spec, &allowed_tools, config, workspace);
    let mut messages: Vec<ChatMessage> = vec![
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
            content: Some(prompt.to_string()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let mut cycles: usize = 0;
    let mut total_tool_calls: usize = 0;
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut provider_usage: HashMap<String, [u64; 2]> = HashMap::new();
    let mut history_snapshot = SubagentHistorySnapshot::default();
    let mut aborted = false;
    let mut timed_out = false;

    // Sub-agent deadline: 0 = unlimited.
    let sa_timeout = config.sub_agent_timeout;
    let unlimited = sa_timeout.is_zero();
    let deadline = tokio::time::Instant::now() + sa_timeout;

    // Mini ReAct loop
    'react: for _cycle in 0..spec.max_turns {
        if cancel.is_cancelled() {
            aborted = true;
            break;
        }

        // Check sub-agent deadline.
        if !unlimited && tokio::time::Instant::now() >= deadline {
            aborted = true;
            timed_out = true;
            break;
        }

        cycles = _cycle + 1;

        // Send progress event to parent
        let _ = live_send(
            parent_live_tx,
            json!({
                "type": "task_progress",
                "task_id": task_id,
                "agent": spec.name,
                "cycle": cycles,
                "phase": "analyze",
            }),
        )
        .await;

        // Prune context before each LLM call to stay within budget.
        // Use message_budget_for_tool_defs which accounts for thinking budget,
        // tool schema tokens, and structural overhead - matching the main loop's
        // request_message_budget_for_runtime but using the sub-agent's actual
        // (filtered) tool definitions instead of all builtins + extras.
        // Let provider/model capabilities decide whether delegated runs should
        // send reasoning controls. This avoids 400s on OpenAI-compatible
        // models that reject `reasoning_effort` or similar fields.
        let think_level = "auto";
        let budget =
            context::message_budget_for_tool_defs(config, &model_id, think_level, &tool_defs);
        context::prune_messages_for_provider(&mut messages, resolved.provider, budget);

        // Call LLM with the sub-agent's isolated context.
        // Agent-level retry: on transient HTTP errors, retry once before aborting.
        let llm_result = 'llm_call: {
            let mut llm_attempt = 0u8;
            loop {
                let (sub_tx, mut sub_rx) = tokio::sync::mpsc::channel::<serde_json::Value>(
                    crate::LIVE_EVENT_CHANNEL_CAPACITY,
                );
                let parent_tx = parent_live_tx.clone();
                let agent_name = spec.name.clone();
                let forward_task_id = task_id.to_string();
                let forward_handle = tokio::spawn(async move {
                    while let Some(mut event) = sub_rx.recv().await {
                        if let Some(obj) = event.as_object_mut() {
                            obj.insert("subagent".into(), json!(agent_name));
                            obj.insert("task_id".into(), json!(forward_task_id));
                        }
                        let _ = live_send(&parent_tx, event).await;
                    }
                });

                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        aborted = true;
                        drop(sub_tx);
                        let _ = forward_handle.await;
                        break 'react;
                    }
                    _ = tokio::time::sleep_until(deadline), if !unlimited => {
                        aborted = true;
                        timed_out = true;
                        drop(sub_tx);
                        let _ = forward_handle.await;
                        break 'react;
                    }
                    result = providers::call_llm_stream_with_tool_mode(
                        http,
                        &resolved,
                        &messages,
                        workspace,
                        config.s3.as_ref(),
                        &sub_tx,
                        think_level,
                        &tool_defs,
                        false,
                        config.max_llm_retries,
                    ) => {
                        drop(sub_tx);
                        let _ = forward_handle.await;
                        result
                    }
                };

                match &result {
                    Err(e) if llm_attempt == 0 && providers::is_transient_llm_error(e) => {
                        llm_attempt += 1;
                        eprintln!("Sub-agent '{}' LLM error, retrying: {e}", spec.name);
                        // Backoff before retry, respecting cancel/deadline.
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => { aborted = true; break 'react; }
                            _ = tokio::time::sleep_until(deadline), if !unlimited => {
                                aborted = true;
                                timed_out = true;
                                break 'react;
                            }
                            _ = tokio::time::sleep(std::time::Duration::from_secs(3)) => {}
                        }
                        continue;
                    }
                    _ => break 'llm_call result,
                }
            }
        };

        match llm_result {
            Ok(resp) => {
                let has_tools = resp.message.has_tool_calls();

                // Accumulate token usage so parent session stats include
                // sub-agent cost. Prefer provider-reported numbers; fall back
                // to local estimates keyed on provider, matching the parent
                // loop's usage accounting pattern.
                let input_used = resp.input_tokens.unwrap_or_else(|| {
                    context::estimate_tokens_for_provider(resolved.provider, &messages) as u64
                });
                let output_used = resp.output_tokens.unwrap_or_else(|| {
                    context::message_token_len_for_provider(resolved.provider, &resp.message) as u64
                });
                accumulate_usage(
                    &mut total_input_tokens,
                    &mut total_output_tokens,
                    &mut provider_usage,
                    &provider_name,
                    input_used,
                    output_used,
                );

                if let Some(thinking) = resp.message.thinking.as_deref() {
                    append_reasoning_snapshot(&mut history_snapshot, cycles, thinking);
                }

                let mut stored_message = resp.message.clone();
                crate::tools::sanitize_chat_message_tool_calls_in_place(&mut stored_message);
                messages.push(stored_message);

                if !has_tools {
                    // Sub-agent finished - extract content
                    break 'react;
                }

                // Execute tool calls - parallel for read-only batches, sequential otherwise.
                if let Some(ref tool_calls) = resp.message.tool_calls {
                    let mut all_read_only = tool_calls.len() > 1;
                    if all_read_only {
                        for tc in tool_calls {
                            if !tools::is_parallelizable_tool_call(
                                &tc.function.name,
                                config,
                                workspace,
                            ) {
                                all_read_only = false;
                                break;
                            }
                        }
                    }

                    if !all_read_only {
                        // ── Sequential path ──────────────────────────────────────
                        for tc in tool_calls {
                            if cancel.is_cancelled() {
                                aborted = true;
                                break 'react;
                            }
                            // Check sub-agent deadline between tool calls.
                            if !unlimited && tokio::time::Instant::now() >= deadline {
                                aborted = true;
                                timed_out = true;
                                break 'react;
                            }

                            // Check tool permission against the pre-computed
                            // allowed list (accounts for mcp_policy + deny overrides).
                            if !allowed_tools.iter().any(|t| t == &tc.function.name) {
                                let result_msg = format!(
                                    "Tool '{}' is not allowed for sub-agent '{}'",
                                    tc.function.name, spec.name
                                );
                                messages.push(ChatMessage {
                                    role: "tool".into(),
                                    content: Some(result_msg),
                                    images: None,
                                    thinking: None,
                                    anthropic_thinking_blocks: None,
                                    tool_calls: None,
                                    tool_call_id: Some(tc.id.clone()),
                                    timestamp: None,
                                });
                                total_tool_calls += 1;
                                continue;
                            }

                            // ── BeforeToolExec hook ──
                            let before_input = ToolHookInput {
                                tool_name: tc.function.name.clone(),
                                tool_args: serde_json::from_str(&tc.function.arguments)
                                    .unwrap_or_else(|_| {
                                        serde_json::Value::String(tc.function.arguments.clone())
                                    }),
                                tool_id: tc.id.clone(),
                                cycle: cycles,
                                workspace: workspace.to_path_buf(),
                                outcome_output: None,
                                outcome_is_error: None,
                                outcome_duration_ms: None,
                            };
                            let hook_output = run_tool_hooks(
                                hooks,
                                agent::HookPoint::BeforeToolExec,
                                before_input,
                                config,
                            )
                            .await;

                            let effective_args = match hook_output {
                                hooks::HookOutput::Reject { reason, events } => {
                                    for ev in events {
                                        let _ = live_send(parent_live_tx, ev).await;
                                    }
                                    total_tool_calls += 1;
                                    messages.push(ChatMessage {
                                        role: "tool".into(),
                                        content: Some(format!("[rejected by hook] {reason}")),
                                        images: None,
                                        thinking: None,
                                        anthropic_thinking_blocks: None,
                                        tool_calls: None,
                                        tool_call_id: Some(tc.id.clone()),
                                        timestamp: None,
                                    });
                                    continue;
                                }
                                hooks::HookOutput::ModifyToolArgs { args } => {
                                    serde_json::to_string(&args)
                                        .unwrap_or_else(|_| tc.function.arguments.clone())
                                }
                                _ => tc.function.arguments.clone(),
                            };

                            // Send tool event to parent
                            let display_args = crate::tools::display_tool_arguments(
                                &tc.function.name,
                                &effective_args,
                            );
                            let _ = live_send(
                                parent_live_tx,
                                json!({
                                    "type": "task_tool",
                                    "task_id": task_id,
                                    "agent": spec.name,
                                    "tool": tc.function.name,
                                    "id": tc.id,
                                    "arguments": crate::truncate(&display_args, 4_000),
                                }),
                            )
                            .await;

                            // Execute the tool, bounded by sub-agent deadline.
                            let tool_started = tokio::time::Instant::now();
                            let (outcome, hit_deadline) = if unlimited {
                                (
                                    execute_subagent_tool_with_live_output(
                                        parent_live_tx,
                                        &cancel,
                                        task_id,
                                        &spec.name,
                                        &tc.function.name,
                                        &tc.id,
                                        &effective_args,
                                        config,
                                        http,
                                        workspace,
                                        false,
                                        replay_ctx.clone(),
                                    )
                                    .await,
                                    false,
                                )
                            } else {
                                tokio::select! {
                                    res = execute_subagent_tool_with_live_output(
                                        parent_live_tx,
                                        &cancel,
                                        task_id,
                                        &spec.name,
                                        &tc.function.name,
                                        &tc.id,
                                        &effective_args,
                                        config,
                                        http,
                                        workspace,
                                        false,
                                        replay_ctx.clone(),
                                    ) => (res, false),
                                    _ = tokio::time::sleep_until(deadline) => {
                                        timed_out = true;
                                        aborted = true;
                                        (
                                            tools::ToolOutcome {
                                                output: format!(
                                                    "Tool '{}' aborted: sub-agent deadline exceeded",
                                                    tc.function.name
                                                ),
                                                is_error: true,
                                                duration_ms: tool_started.elapsed().as_millis() as u64,
                                                subagent_snapshot: None,
                                            },
                                            true,
                                        )
                                    }
                                }
                            };

                            total_tool_calls += 1;

                            let outcome = apply_after_tool_exec_hook(
                                hooks,
                                config,
                                workspace,
                                cycles,
                                &tc.function.name,
                                &effective_args,
                                &tc.id,
                                outcome,
                            )
                            .await;

                            emit_subagent_tool_result_event(
                                parent_live_tx,
                                task_id,
                                &spec.name,
                                &tc.function.name,
                                &tc.id,
                                &outcome,
                            )
                            .await;

                            history_snapshot.tools.push(SubagentToolHistorySnapshot {
                                id: tc.id.clone(),
                                name: tc.function.name.clone(),
                                arguments: truncated_option(
                                    &display_args,
                                    MAX_SNAPSHOT_TOOL_ARGS_CHARS,
                                ),
                                result: truncated_option(
                                    &outcome.output,
                                    MAX_SNAPSHOT_TOOL_RESULT_CHARS,
                                ),
                                is_error: outcome.is_error,
                                duration_ms: outcome.duration_ms,
                            });

                            messages.push(ChatMessage {
                                role: "tool".into(),
                                content: Some(outcome.output),
                                images: None,
                                thinking: None,
                                anthropic_thinking_blocks: None,
                                tool_calls: None,
                                tool_call_id: Some(tc.id.clone()),
                                timestamp: None,
                            });

                            if hit_deadline {
                                break 'react;
                            }
                        }
                    } else {
                        // ── Parallel path for read-only tool batches ─────────────
                        // Covers built-in read-only tools plus MCP tools whose
                        // descriptors are conservatively classified as read-only.
                        // MCP calls in this path use isolated sessions so they
                        // do not serialize behind the shared session cache.
                        // Mirrors the parent run_act_phase() 4-phase pattern:
                        //   1. Sequential hook evaluation
                        //   2. Send task_tool events
                        //   3. Parallel execution bounded by deadline
                        //   4. Sequential result recording

                        // Phase 1: Evaluate BeforeToolExec hooks sequentially.
                        struct SubHookEval {
                            effective_args: Option<String>,
                            rejected_output: Option<String>,
                            reject_events: Vec<serde_json::Value>,
                            disallowed: bool,
                        }
                        let mut hook_evals: Vec<SubHookEval> = Vec::with_capacity(tool_calls.len());
                        for tc in tool_calls {
                            if !allowed_tools.iter().any(|t| t == &tc.function.name) {
                                hook_evals.push(SubHookEval {
                                    effective_args: None,
                                    rejected_output: Some(format!(
                                        "Tool '{}' is not allowed for sub-agent '{}'",
                                        tc.function.name, spec.name
                                    )),
                                    reject_events: Vec::new(),
                                    disallowed: true,
                                });
                                continue;
                            }
                            let before_input = ToolHookInput {
                                tool_name: tc.function.name.clone(),
                                tool_args: serde_json::from_str(&tc.function.arguments)
                                    .unwrap_or_else(|_| {
                                        serde_json::Value::String(tc.function.arguments.clone())
                                    }),
                                tool_id: tc.id.clone(),
                                cycle: cycles,
                                workspace: workspace.to_path_buf(),
                                outcome_output: None,
                                outcome_is_error: None,
                                outcome_duration_ms: None,
                            };
                            let hook_output = run_tool_hooks(
                                hooks,
                                agent::HookPoint::BeforeToolExec,
                                before_input,
                                config,
                            )
                            .await;
                            hook_evals.push(match hook_output {
                                hooks::HookOutput::Reject { reason, events } => SubHookEval {
                                    effective_args: None,
                                    rejected_output: Some(format!("[rejected by hook] {reason}")),
                                    reject_events: events,
                                    disallowed: false,
                                },
                                hooks::HookOutput::ModifyToolArgs { args } => SubHookEval {
                                    effective_args: Some(
                                        serde_json::to_string(&args)
                                            .unwrap_or_else(|_| tc.function.arguments.clone()),
                                    ),
                                    rejected_output: None,
                                    reject_events: Vec::new(),
                                    disallowed: false,
                                },
                                _ => SubHookEval {
                                    effective_args: Some(tc.function.arguments.clone()),
                                    rejected_output: None,
                                    reject_events: Vec::new(),
                                    disallowed: false,
                                },
                            });
                        }

                        // Phase 2: Send task_tool events and hook reject events.
                        for (tc, he) in tool_calls.iter().zip(hook_evals.iter()) {
                            if cancel.is_cancelled() {
                                aborted = true;
                                break 'react;
                            }
                            if he.disallowed || he.rejected_output.is_some() {
                                for ev in &he.reject_events {
                                    let _ = live_send(parent_live_tx, ev.clone()).await;
                                }
                                continue;
                            }
                            let display_args = crate::tools::display_tool_arguments(
                                &tc.function.name,
                                he.effective_args
                                    .as_deref()
                                    .unwrap_or(&tc.function.arguments),
                            );
                            let _ = live_send(
                                parent_live_tx,
                                json!({
                                    "type": "task_tool",
                                    "task_id": task_id,
                                    "agent": spec.name,
                                    "tool": tc.function.name,
                                    "id": tc.id,
                                    "arguments": crate::truncate(&display_args, 4_000),
                                }),
                            )
                            .await;
                        }

                        // Phase 3: Launch tool futures concurrently and preserve any
                        // completed results if cancellation or the sub-agent deadline hits.
                        let batch_started = tokio::time::Instant::now();
                        let tool_futures: Vec<_> = tool_calls
                            .iter()
                            .zip(hook_evals.iter())
                            .map(|(tc, he)| {
                                if he.rejected_output.is_some() || he.disallowed {
                                    return Box::pin(async { None })
                                        as std::pin::Pin<
                                            Box<
                                                dyn std::future::Future<
                                                        Output = Option<tools::ToolOutcome>,
                                                    > + Send,
                                            >,
                                        >;
                                }
                                let args = he
                                    .effective_args
                                    .as_deref()
                                    .unwrap_or(&tc.function.arguments)
                                    .to_string();
                                let name = tc.function.name.clone();
                                let cfg = config.clone();
                                let cl = http.clone();
                                let ws = workspace.to_path_buf();
                                let parent_live_tx = parent_live_tx.clone();
                                let task_id = task_id.to_string();
                                let agent_name = spec.name.clone();
                                let tool_id = tc.id.clone();
                                let replay_ctx = replay_ctx.clone();
                                let cancel = cancel.clone();
                                Box::pin(async move {
                                    Some(
                                        execute_subagent_tool_with_live_output(
                                            &parent_live_tx,
                                            &cancel,
                                            &task_id,
                                            &agent_name,
                                            &name,
                                            &tool_id,
                                            &args,
                                            &cfg,
                                            &cl,
                                            &ws,
                                            true,
                                            replay_ctx,
                                        )
                                        .await,
                                    )
                                })
                            })
                            .collect();

                        let batch_result = collect_parallel_tool_results(
                            tool_futures,
                            &cancel,
                            (!unlimited).then_some(deadline),
                        )
                        .await;

                        if batch_result.interrupted {
                            aborted = true;
                            timed_out |= batch_result.timed_out;
                        }

                        // Phase 4: Record results sequentially, apply AfterToolExec hooks.
                        for (tc, (result_opt, he)) in tool_calls
                            .iter()
                            .zip(batch_result.results.into_iter().zip(hook_evals.into_iter()))
                        {
                            total_tool_calls += 1;
                            if let Some(rejected_msg) = he.rejected_output {
                                messages.push(ChatMessage {
                                    role: "tool".into(),
                                    content: Some(rejected_msg),
                                    images: None,
                                    thinking: None,
                                    anthropic_thinking_blocks: None,
                                    tool_calls: None,
                                    tool_call_id: Some(tc.id.clone()),
                                    timestamp: None,
                                });
                                continue;
                            }
                            let eff_args = he
                                .effective_args
                                .as_deref()
                                .unwrap_or(&tc.function.arguments);
                            let outcome = finalize_parallel_batch_outcome(
                                hooks,
                                config,
                                workspace,
                                cycles,
                                &tc.function.name,
                                eff_args,
                                &tc.id,
                                result_opt,
                                batch_result.interrupted,
                                batch_result.timed_out,
                                batch_started.elapsed().as_millis() as u64,
                            )
                            .await;
                            emit_subagent_tool_result_event(
                                parent_live_tx,
                                task_id,
                                &spec.name,
                                &tc.function.name,
                                &tc.id,
                                &outcome,
                            )
                            .await;
                            history_snapshot.tools.push(SubagentToolHistorySnapshot {
                                id: tc.id.clone(),
                                name: tc.function.name.clone(),
                                arguments: truncated_option(
                                    &crate::tools::display_tool_arguments(
                                        &tc.function.name,
                                        eff_args,
                                    ),
                                    MAX_SNAPSHOT_TOOL_ARGS_CHARS,
                                ),
                                result: truncated_option(
                                    &outcome.output,
                                    MAX_SNAPSHOT_TOOL_RESULT_CHARS,
                                ),
                                is_error: outcome.is_error,
                                duration_ms: outcome.duration_ms,
                            });
                            messages.push(ChatMessage {
                                role: "tool".into(),
                                content: Some(outcome.output),
                                images: None,
                                thinking: None,
                                anthropic_thinking_blocks: None,
                                tool_calls: None,
                                tool_call_id: Some(tc.id.clone()),
                                timestamp: None,
                            });
                        }

                        if batch_result.interrupted {
                            break 'react;
                        }
                    } // end parallel path
                }
            }
            Err(error) => {
                // LLM error - abort sub-agent.
                // Do NOT send task_failed here; execute_task_tool() in
                // runtime_loop.rs sends the final event based on outcome.aborted.
                history_snapshot.cycles = cycles;
                history_snapshot.tool_calls = total_tool_calls;
                history_snapshot.input_tokens = total_input_tokens;
                history_snapshot.output_tokens = total_output_tokens;
                history_snapshot.success = false;
                history_snapshot.error = Some(
                    truncate(
                        &format!("Sub-agent '{}' failed: {}", spec.name, error),
                        MAX_SNAPSHOT_RESULT_CHARS,
                    )
                    .to_string(),
                );
                return SubAgentOutcome {
                    result: format!("Sub-agent '{}' failed: {}", spec.name, error),
                    cycles,
                    tool_calls: total_tool_calls,
                    aborted: true,
                    total_input_tokens,
                    total_output_tokens,
                    provider_usage,
                    history_snapshot,
                };
            }
        }
    }

    if !aborted && needs_forced_final_response(&messages) {
        match request_forced_final_response(
            &model_id, &resolved, config, http, workspace, &messages,
        )
        .await
        {
            Ok((forced_messages, forced_response))
                if !forced_response.content.trim().is_empty() =>
            {
                let final_message = ChatMessage {
                    role: "assistant".into(),
                    content: Some(forced_response.content),
                    images: None,
                    thinking: None,
                    anthropic_thinking_blocks: None,
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: None,
                };
                let input_used = forced_response.input_tokens.unwrap_or_else(|| {
                    context::estimate_tokens_for_provider(resolved.provider, &forced_messages)
                        as u64
                });
                let output_used = forced_response.output_tokens.unwrap_or_else(|| {
                    context::message_token_len_for_provider(resolved.provider, &final_message)
                        as u64
                });
                accumulate_usage(
                    &mut total_input_tokens,
                    &mut total_output_tokens,
                    &mut provider_usage,
                    &provider_name,
                    input_used,
                    output_used,
                );
                messages.push(final_message);
            }
            Ok(_) => {
                eprintln!(
                    "Sub-agent '{}' forced final response returned empty content",
                    spec.name
                );
            }
            Err(error) => {
                eprintln!(
                    "Sub-agent '{}' forced final response failed: {}",
                    spec.name, error
                );
            }
        }
    }

    let final_content = final_assistant_content(&messages);
    let final_tool_content = final_tool_content(&messages);

    let result = if timed_out {
        let partial = truncate(&final_content, MAX_RESULT_CHARS.saturating_sub(200));
        if partial.is_empty() {
            format!(
                "Sub-agent '{}' timed out after {}s ({} cycles, {} tool calls) with no output.",
                spec.name,
                sa_timeout.as_secs(),
                cycles,
                total_tool_calls
            )
        } else {
            format!(
                "Sub-agent '{}' timed out after {}s ({} cycles, {} tool calls). Partial result:\n\n{}",
                spec.name,
                sa_timeout.as_secs(),
                cycles,
                total_tool_calls,
                partial
            )
        }
    } else if !final_content.is_empty() {
        truncate(&final_content, MAX_RESULT_CHARS).to_string()
    } else if !final_tool_content.is_empty() {
        truncate(&final_tool_content, MAX_RESULT_CHARS).to_string()
    } else {
        format!(
            "Sub-agent '{}' completed {} cycles with {} tool calls but produced no final output.",
            spec.name, cycles, total_tool_calls
        )
    };

    history_snapshot.cycles = cycles;
    history_snapshot.tool_calls = total_tool_calls;
    history_snapshot.input_tokens = total_input_tokens;
    history_snapshot.output_tokens = total_output_tokens;
    history_snapshot.success = !aborted;
    if aborted {
        history_snapshot.error = Some(truncate(&result, MAX_SNAPSHOT_RESULT_CHARS).to_string());
    } else {
        history_snapshot.result_excerpt =
            Some(truncate(&result, MAX_SNAPSHOT_RESULT_CHARS).to_string());
    }

    SubAgentOutcome {
        result,
        cycles,
        tool_calls: total_tool_calls,
        aborted,
        total_input_tokens,
        total_output_tokens,
        provider_usage,
        history_snapshot,
    }
}

fn build_subagent_system_prompt(
    spec: &SubAgentSpec,
    allowed_tools: &[String],
    config: &Config,
    workspace: &Path,
) -> String {
    let tool_list = if allowed_tools.is_empty() {
        "(no tools available)".to_string()
    } else {
        // Build a lookup of MCP tool descriptions from cache.
        let mcp_descriptors = tools::mcp::cached_list_tools(config, workspace);
        let mcp_desc_map: std::collections::HashMap<&str, &str> = mcp_descriptors
            .iter()
            .map(|d| (d.exposed_name.as_str(), d.description.as_str()))
            .collect();

        allowed_tools
            .iter()
            .enumerate()
            .map(|(i, name)| {
                // Try built-in description first, then MCP description.
                let desc = crate::tools::tool_specs()
                    .iter()
                    .find(|ts| ts.name == name)
                    .map(|ts| ts.description)
                    .or_else(|| mcp_desc_map.get(name.as_str()).copied())
                    .unwrap_or("");
                format!("{}. **{}** - {}", i + 1, name, desc)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "{}\n\n---\n\n\
         ## Sub-Agent Context\n\
         You are running as a sub-agent with isolated context. \
         Complete the delegated task and provide your final answer. \
         Do not ask the user questions - work with what you have.\n\n\
         ## Available Tools\n\
         {}\n\n\
         ## Constraints\n\
         - Max cycles: {}\n\
         - You cannot delegate to other sub-agents (no `task` tool).\n\
         - Provide your final answer as a clear, well-structured response.",
        spec.system_prompt, tool_list, spec.max_turns,
    )
}

fn build_filtered_tool_defs(
    allowed_tools: &[String],
    config: &Config,
    workspace: &Path,
    provider: crate::config::Provider,
) -> Vec<serde_json::Value> {
    let all_specs = crate::tools::tool_specs();
    let mut defs: Vec<serde_json::Value> = all_specs
        .iter()
        .filter(|ts| allowed_tools.iter().any(|a| a == ts.name))
        .map(|spec| match provider {
            crate::config::Provider::OpenAI
            | crate::config::Provider::OpenAIResponses
            | crate::config::Provider::Ollama => {
                json!({
                    "type": "function",
                    "function": {
                        "name": spec.name,
                        "description": spec.description,
                        "parameters": (spec.parameters)(),
                    }
                })
            }
            crate::config::Provider::Anthropic => {
                json!({
                    "name": spec.name,
                    "description": spec.description,
                    "input_schema": (spec.parameters)(),
                })
            }
            crate::config::Provider::Gemini => {
                json!({
                    "name": spec.name,
                    "description": spec.description,
                    "parameters": crate::tools::gemini_tool_parameters((spec.parameters)()),
                })
            }
        })
        .collect();

    // Append MCP tool definitions from cache.
    let mcp_descriptors = tools::mcp::cached_list_tools(config, workspace);
    for descriptor in mcp_descriptors {
        if !allowed_tools.iter().any(|a| a == &descriptor.exposed_name) {
            continue;
        }
        let def = match provider {
            crate::config::Provider::OpenAI
            | crate::config::Provider::OpenAIResponses
            | crate::config::Provider::Ollama => {
                json!({
                    "type": "function",
                    "function": {
                        "name": descriptor.exposed_name,
                        "description": descriptor.description,
                        "parameters": descriptor.input_schema,
                    }
                })
            }
            crate::config::Provider::Anthropic => {
                json!({
                    "name": descriptor.exposed_name,
                    "description": descriptor.description,
                    "input_schema": descriptor.input_schema,
                })
            }
            crate::config::Provider::Gemini => {
                json!({
                    "name": descriptor.exposed_name,
                    "description": descriptor.description,
                    "parameters": crate::tools::gemini_tool_parameters(descriptor.input_schema),
                })
            }
        };
        defs.push(def);
    }

    defs
}

async fn execute_subagent_tool(
    name: &str,
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
    isolated_mcp_session: bool,
    event_tx: Option<tools::ToolEventSender>,
    bounded_event_tx: Option<tools::BoundedToolEventSender>,
) -> tools::ToolOutcome {
    let mcp_result = if isolated_mcp_session {
        tools::mcp::execute_tool_isolated(name, args_str, config, workspace).await
    } else {
        tools::mcp::execute_tool(name, args_str, config, workspace).await
    };

    if let Some(result) = mcp_result {
        result
    } else {
        tools::execute_tool_with_bounded_live_events(
            name,
            args_str,
            config,
            http,
            workspace,
            event_tx,
            bounded_event_tx,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Provider;
    use std::{
        collections::HashMap,
        path::PathBuf,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    fn test_config() -> Config {
        Config {
            api_key: String::new(),
            api_base: "https://api.openai.com/v1".to_string(),
            model: "openai/gpt-4o".to_string(),
            fast_model: None,
            sub_agent_model: None,
            sub_agent_model_overrides: Default::default(),
            memory_model: None,
            reflection_model: None,
            context_model: None,
            provider: Provider::OpenAI,
            anthropic_prompt_caching: false,
            openai_stream_include_usage: false,
            providers: HashMap::new(),
            mcp_servers: HashMap::new(),
            port: 18989,
            max_context_tokens: 32_000,
            exec_timeout: Duration::from_secs(30),
            tool_timeout: Duration::from_secs(30),
            sub_agent_timeout: Duration::from_secs(300),
            max_llm_retries: 2,
            max_output_bytes: 50 * 1024,
            max_file_bytes: 200 * 1024,
            structured_memory: false,
            daily_reflection: false,
            s3: None,
            enable_state_digest: true,
        }
    }

    fn unique_temp_workspace(prefix: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}"))
    }

    #[tokio::test]
    async fn execute_subagent_tool_with_live_output_preserves_queued_events() {
        let workspace = unique_temp_workspace("lingclaw-subagent-live-output-full");
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        tokio::fs::create_dir_all(&workspace)
            .await
            .expect("workspace should be created");

        let args = if cfg!(windows) {
            serde_json::json!({
                "program": "cmd",
                "args": ["/C", "echo subagent-live-full"],
            })
        } else {
            serde_json::json!({
                "program": "sh",
                "args": ["-c", "printf 'subagent-live-full'"],
            })
        };
        let (live_tx, mut live_rx) =
            tokio::sync::mpsc::channel::<serde_json::Value>(crate::LIVE_EVENT_CHANNEL_CAPACITY);
        live_tx
            .send(serde_json::json!({"type":"sentinel"}))
            .await
            .expect("sentinel should queue");

        let outcome = tokio::time::timeout(
            Duration::from_millis(250),
            execute_subagent_tool_with_live_output(
                &live_tx,
                &CancellationToken::new(),
                "task-1",
                "agent-1",
                tools::TOOL_NAME_EXEC,
                "call-1",
                &serde_json::to_string(&args).expect("args should serialize"),
                &test_config(),
                &Client::new(),
                &workspace,
                false,
                None,
            ),
        )
        .await
        .expect("subagent exec wrapper should not block when live events are already queued");

        assert!(
            !outcome.is_error,
            "subagent exec wrapper should still succeed"
        );
        assert_eq!(
            live_rx.try_recv().expect("sentinel should remain queued")["type"],
            "sentinel"
        );
        let tool_output = live_rx
            .try_recv()
            .expect("tool output should remain queued after the sentinel");
        assert_eq!(tool_output["type"].as_str(), Some("tool_output"));
        assert_eq!(tool_output["id"].as_str(), Some("call-1"));

        let _ = tokio::fs::remove_dir_all(&workspace).await;
    }

    #[tokio::test]
    async fn execute_subagent_tool_with_live_output_returns_when_live_queue_is_full() {
        let workspace = unique_temp_workspace("lingclaw-subagent-live-output-blocked");
        let _ = tokio::fs::remove_dir_all(&workspace).await;
        tokio::fs::create_dir_all(&workspace)
            .await
            .expect("workspace should be created");

        let args = if cfg!(windows) {
            serde_json::json!({
                "program": "cmd",
                "args": ["/C", "echo subagent-live-blocked"],
            })
        } else {
            serde_json::json!({
                "program": "sh",
                "args": ["-c", "printf 'subagent-live-blocked'"],
            })
        };
        let (live_tx, mut live_rx) =
            tokio::sync::mpsc::channel::<serde_json::Value>(crate::LIVE_EVENT_CHANNEL_CAPACITY);
        for _ in 0..crate::LIVE_EVENT_CHANNEL_CAPACITY {
            live_tx
                .send(serde_json::json!({"type":"sentinel"}))
                .await
                .expect("sentinel should queue");
        }

        let outcome = tokio::time::timeout(
            Duration::from_millis(250),
            execute_subagent_tool_with_live_output(
                &live_tx,
                &CancellationToken::new(),
                "task-1",
                "agent-1",
                tools::TOOL_NAME_EXEC,
                "call-1",
                &serde_json::to_string(&args).expect("args should serialize"),
                &test_config(),
                &Client::new(),
                &workspace,
                false,
                None,
            ),
        )
        .await
        .expect("subagent exec wrapper should not wait on a full live queue");

        assert!(
            !outcome.is_error,
            "subagent exec wrapper should still succeed"
        );
        assert_eq!(
            live_rx.try_recv().expect("sentinel should remain queued")["type"],
            "sentinel"
        );

        let _ = tokio::fs::remove_dir_all(&workspace).await;
    }
}
