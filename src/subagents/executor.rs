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

    // Build filtered tool definitions for this sub-agent.
    let allowed_tools = super::filter_tools_for_agent(spec);
    let tool_defs = build_filtered_tool_defs(&allowed_tools, resolved.provider);

    // Build isolated message history.
    let system_prompt = build_subagent_system_prompt(spec, &allowed_tools, config);
    let mut messages: Vec<ChatMessage> = vec![
        ChatMessage {
            role: "system".into(),
            content: Some(system_prompt),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(prompt.to_string()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let mut cycles: usize = 0;
    let mut total_tool_calls: usize = 0;
    let mut aborted = false;

    // Mini ReAct loop
    'react: for _cycle in 0..spec.max_turns {
        if cancel.is_cancelled() {
            aborted = true;
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

        // Create a per-cycle channel for LLM streaming events.
        // The forwarder task tags events with the sub-agent name and relays them.
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

        // Prune context before each LLM call to stay within budget.
        // Use message_budget_for_tool_defs which accounts for thinking budget,
        // tool schema tokens, and structural overhead — matching the main loop's
        // request_message_budget_for_runtime but using the sub-agent's actual
        // (filtered) tool definitions instead of all builtins + extras.
        let think_level = "medium";
        let budget =
            context::message_budget_for_tool_defs(config, &model_id, think_level, &tool_defs);
        context::prune_messages_for_provider(&mut messages, resolved.provider, budget);

        // Call LLM with the sub-agent's isolated context
        let llm_result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                aborted = true;
                drop(sub_tx);
                let _ = forward_handle.await;
                break 'react;
            }
            result = providers::call_llm_stream(
                http,
                &resolved,
                &messages,
                &sub_tx,
                think_level,
                &tool_defs,
            ) => {
                drop(sub_tx);
                let _ = forward_handle.await;
                result
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

                        // Check tool permission
                        if !spec.tools.is_allowed(&tc.function.name) {
                            let result_msg = format!(
                                "Tool '{}' is not allowed for sub-agent '{}'",
                                tc.function.name, spec.name
                            );
                            messages.push(ChatMessage {
                                role: "tool".into(),
                                content: Some(result_msg),
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

                        // Execute the tool
                        let mut outcome = execute_subagent_tool(
                            &tc.function.name,
                            &effective_args,
                            config,
                            http,
                            workspace,
                        )
                        .await;

                        total_tool_calls += 1;

                        // ── AfterToolExec hook ──
                        let after_input = ToolHookInput {
                            tool_name: tc.function.name.clone(),
                            tool_args: serde_json::from_str(&effective_args).unwrap_or_else(|_| {
                                serde_json::Value::String(effective_args.clone())
                            }),
                            tool_id: tc.id.clone(),
                            cycle: cycles,
                            workspace: workspace.to_path_buf(),
                            outcome_output: Some(outcome.output.clone()),
                            outcome_is_error: Some(outcome.is_error),
                            outcome_duration_ms: Some(outcome.duration_ms),
                        };
                        let after_output = run_tool_hooks(
                            hooks,
                            agent::HookPoint::AfterToolExec,
                            after_input,
                            config,
                        )
                        .await;
                        if let hooks::HookOutput::ModifyToolResult { result } = after_output {
                            outcome.output = result;
                        }

                        messages.push(ChatMessage {
                            role: "tool".into(),
                            content: Some(outcome.output),
                            tool_calls: None,
                            tool_call_id: Some(tc.id.clone()),
                            timestamp: None,
                        });
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
        .unwrap_or_else(|| {
            format!(
                "Sub-agent '{}' completed {} cycles with {} tool calls but produced no final output.",
                spec.name, cycles, total_tool_calls
            )
        });

    let result = truncate(&final_content, MAX_RESULT_CHARS).to_string();

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
    _config: &Config,
) -> String {
    let tool_list = if allowed_tools.is_empty() {
        "(no tools available)".to_string()
    } else {
        allowed_tools
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let desc = crate::tools::tool_specs()
                    .iter()
                    .find(|ts| ts.name == name)
                    .map(|ts| ts.description)
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
    provider: crate::config::Provider,
) -> Vec<serde_json::Value> {
    let all_specs = crate::tools::tool_specs();
    all_specs
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
        .collect()
}

/// Execute a tool within the sub-agent context.
/// Uses the same tool registry as the parent, but goes through the built-in
/// execute path only (no MCP tools for sub-agents to keep isolation simple).
async fn execute_subagent_tool(
    name: &str,
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
) -> tools::ToolOutcome {
    tools::execute_tool(name, args_str, config, http, workspace).await
}
