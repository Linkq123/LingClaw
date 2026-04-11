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

use std::path::Path;

use reqwest::Client;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::SubAgentSpec;
use crate::{
    ChatMessage, Config, LiveTx, agent, context,
    hooks::{self, HookRegistry, ToolHookInput, run_tool_hooks},
    live_send, providers, tools, truncate,
};

/// Maximum characters in the sub-agent's final result returned to the parent.
const MAX_RESULT_CHARS: usize = 30_000;

/// Apply the sub-agent AfterToolExec hook to a real or synthetic tool outcome.
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
}

/// Resolve which model a sub-agent should use.
/// Sub-agents always use the runtime config: `sub_agent_model` when set,
/// otherwise the primary model.
pub(crate) fn resolve_subagent_model(config: &Config) -> &str {
    config.sub_agent_model.as_deref().unwrap_or(&config.model)
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
) -> SubAgentOutcome {
    let model_id = resolve_subagent_model(config).to_string();
    let resolved = config.resolve_model(&model_id);

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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(prompt.to_string()),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let mut cycles: usize = 0;
    let mut total_tool_calls: usize = 0;
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
                "agent": spec.name,
                "cycle": cycles,
                "phase": "analyze",
            }),
        )
        .await;

        // Prune context before each LLM call to stay within budget.
        // Use message_budget_for_tool_defs which accounts for thinking budget,
        // tool schema tokens, and structural overhead — matching the main loop's
        // request_message_budget_for_runtime but using the sub-agent's actual
        // (filtered) tool definitions instead of all builtins + extras.
        let think_level = "medium";
        let budget =
            context::message_budget_for_tool_defs(config, &model_id, think_level, &tool_defs);
        context::prune_messages_for_provider(&mut messages, resolved.provider, budget);

        // Call LLM with the sub-agent's isolated context.
        // Agent-level retry: on transient HTTP errors, retry once before aborting.
        let llm_result = 'llm_call: {
            let mut llm_attempt = 0u8;
            loop {
                let (sub_tx, mut sub_rx) = tokio::sync::mpsc::channel::<serde_json::Value>(64);
                let parent_tx = parent_live_tx.clone();
                let agent_name = spec.name.clone();
                let forward_handle = tokio::spawn(async move {
                    while let Some(mut event) = sub_rx.recv().await {
                        if let Some(obj) = event.as_object_mut() {
                            obj.insert("subagent".into(), json!(agent_name));
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
                    result = providers::call_llm_stream(
                        http,
                        &resolved,
                        &messages,
                        workspace,
                        config.s3.as_ref(),
                        &sub_tx,
                        think_level,
                        &tool_defs,
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

                messages.push(resp.message.clone());

                if !has_tools {
                    // Sub-agent finished — extract content
                    break 'react;
                }

                // Execute tool calls sequentially
                if let Some(ref tool_calls) = resp.message.tool_calls {
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
                            tool_args: serde_json::from_str(&tc.function.arguments).unwrap_or_else(
                                |_| serde_json::Value::String(tc.function.arguments.clone()),
                            ),
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
                        let _ = live_send(
                            parent_live_tx,
                            json!({
                                "type": "task_tool",
                                "agent": spec.name,
                                "tool": tc.function.name,
                                "id": tc.id,
                            }),
                        )
                        .await;

                        // Execute the tool, bounded by sub-agent deadline.
                        let tool_started = tokio::time::Instant::now();
                        let (outcome, hit_deadline) = if unlimited {
                            (
                                execute_subagent_tool(
                                    &tc.function.name,
                                    &effective_args,
                                    config,
                                    http,
                                    workspace,
                                )
                                .await,
                                false,
                            )
                        } else {
                            tokio::select! {
                                res = execute_subagent_tool(
                                    &tc.function.name,
                                    &effective_args,
                                    config,
                                    http,
                                    workspace,
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

                        messages.push(ChatMessage {
                            role: "tool".into(),
                            content: Some(outcome.output),
                            images: None,
                            tool_calls: None,
                            tool_call_id: Some(tc.id.clone()),
                            timestamp: None,
                        });

                        if hit_deadline {
                            break 'react;
                        }
                    }
                }
            }
            Err(error) => {
                // LLM error — abort sub-agent.
                // Do NOT send task_failed here; execute_task_tool() in
                // runtime_loop.rs sends the final event based on outcome.aborted.
                return SubAgentOutcome {
                    result: format!("Sub-agent '{}' failed: {}", spec.name, error),
                    cycles,
                    tool_calls: total_tool_calls,
                    aborted: true,
                };
            }
        }
    }

    // Extract final result from the last assistant message
    let final_content = messages
        .iter()
        .rev()
        .find(|m| m.role == "assistant" && m.has_nonempty_content())
        .and_then(|m| m.content.clone())
        .unwrap_or_default();

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
    } else if final_content.is_empty() {
        format!(
            "Sub-agent '{}' completed {} cycles with {} tool calls but produced no final output.",
            spec.name, cycles, total_tool_calls
        )
    } else {
        truncate(&final_content, MAX_RESULT_CHARS).to_string()
    };

    SubAgentOutcome {
        result,
        cycles,
        tool_calls: total_tool_calls,
        aborted,
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
                format!("{}. **{}** — {}", i + 1, name, desc)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "{}\n\n---\n\n\
         ## Sub-Agent Context\n\
         You are running as a sub-agent with isolated context. \
         Complete the delegated task and provide your final answer. \
         Do not ask the user questions — work with what you have.\n\n\
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
            crate::config::Provider::OpenAI | crate::config::Provider::Ollama => {
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
        })
        .collect();

    // Append MCP tool definitions from cache.
    let mcp_descriptors = tools::mcp::cached_list_tools(config, workspace);
    for descriptor in mcp_descriptors {
        if !allowed_tools.iter().any(|a| a == &descriptor.exposed_name) {
            continue;
        }
        let def = match provider {
            crate::config::Provider::OpenAI | crate::config::Provider::Ollama => {
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
        };
        defs.push(def);
    }

    defs
}

/// Execute a tool within the sub-agent context.
/// Tries MCP tools first (matching the main loop pattern), then falls back
/// to the built-in tool registry.
async fn execute_subagent_tool(
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
