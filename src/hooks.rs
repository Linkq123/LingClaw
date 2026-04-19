use std::{collections::HashMap, future::Future, path::PathBuf, pin::Pin};

use reqwest::Client;
use serde_json::json;
use tokio::sync::Mutex;

use crate::{
    ChatMessage, Session, agent,
    config::{Config, Provider},
    context::{
        USAGE_ROLE_CONTEXT, UsageUpdate, apply_usage_update, build_usage_labels,
        context_input_budget_for_model, estimate_tokens_for_provider, turn_len,
    },
    providers, truncate,
};

// ── Hook Infrastructure ──────────────────────────────────────────────────────

/// Owned snapshot of session state for hook execution (lock-free).
#[allow(dead_code)]
pub(crate) struct HookInput {
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) model: String,
    pub(crate) provider: Provider,
    pub(crate) workspace: PathBuf,
    pub(crate) input_budget: usize,
    pub(crate) cycle: usize,
}

/// Mutations a hook can request.
#[allow(dead_code)]
pub(crate) enum HookOutput {
    /// No changes needed.
    NoOp,
    /// Replace session messages and optionally emit frontend events.
    ReplaceMessages {
        messages: Vec<ChatMessage>,
        events: Vec<serde_json::Value>,
        usage: Option<UsageUpdate>,
    },
    /// Modify tool arguments before execution (BeforeToolExec only).
    ModifyToolArgs { args: serde_json::Value },
    /// Modify the tool result string after execution (AfterToolExec only).
    ModifyToolResult { result: String },
    /// Reject tool execution entirely (BeforeToolExec only).
    Reject {
        reason: String,
        events: Vec<serde_json::Value>,
    },
    /// Modify LLM call parameters (BeforeLlmCall only).
    ModifyLlmParams {
        /// Extra text appended to the system prompt.
        extra_system: Option<String>,
        /// Override the effective think level.
        think_override: Option<String>,
    },
}

/// Owned snapshot for tool-level hook execution (lock-free).
pub(crate) struct ToolHookInput {
    pub(crate) tool_name: String,
    pub(crate) tool_args: serde_json::Value,
    pub(crate) tool_id: String,
    pub(crate) cycle: usize,
    pub(crate) workspace: PathBuf,
    /// Present only for `AfterToolExec`.
    pub(crate) outcome_output: Option<String>,
    /// Present only for `AfterToolExec`.
    pub(crate) outcome_is_error: Option<bool>,
    /// Present only for `AfterToolExec`.
    pub(crate) outcome_duration_ms: Option<u64>,
}

/// Owned snapshot for LLM-call-level hook execution (lock-free).
pub(crate) struct LlmHookInput {
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) model: String,
    pub(crate) think_level: String,
    pub(crate) cycle: usize,
    pub(crate) tool_count: usize,
}

/// Owned snapshot for command hook execution.
pub(crate) struct CommandHookInput {
    pub(crate) command: String,
    pub(crate) args: String,
    pub(crate) result_type: String,
    pub(crate) session_id: String,
}

/// Agent lifecycle hook.
///
/// Hooks follow a two-phase pattern to avoid holding the session lock during I/O:
///   1. `should_run` — fast eligibility check, called **under** session lock.
///   2. `run` — async execution, called **without** session lock.
pub(crate) trait AgentHook: Send + Sync {
    /// Human-readable name for logging.
    #[allow(dead_code)]
    fn name(&self) -> &'static str;

    /// Which lifecycle point this hook fires at.
    fn point(&self) -> agent::HookPoint;

    /// Fast eligibility check. Called under session lock — must not do I/O.
    fn should_run(
        &self,
        messages: &[ChatMessage],
        provider: Provider,
        input_budget: usize,
        cycle: usize,
    ) -> bool;

    /// Execute the hook asynchronously. Called WITHOUT session lock.
    fn run<'a>(
        &'a self,
        input: HookInput,
        config: &'a Config,
        http: &'a Client,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>>;

    // ── Tool-level hooks (opt-in) ────────────────────────────────────────

    /// Fast eligibility check for tool hooks (`BeforeToolExec` / `AfterToolExec`).
    /// Default: not interested in tool events.
    fn should_run_tool(&self, _tool_name: &str, _point: agent::HookPoint) -> bool {
        false
    }

    /// Execute a tool-level hook. Called **without** session lock.
    fn run_tool<'a>(
        &'a self,
        _input: ToolHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async { HookOutput::NoOp })
    }

    // ── LLM-level hooks (opt-in) ─────────────────────────────────────────

    /// Fast eligibility check for `BeforeLlmCall`.
    fn should_run_llm(&self, _cycle: usize) -> bool {
        false
    }

    /// Execute an LLM-level hook. Called **without** session lock.
    fn run_llm<'a>(
        &'a self,
        _input: LlmHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async { HookOutput::NoOp })
    }

    // ── Command hooks (opt-in) ───────────────────────────────────────────

    /// Fast eligibility check for `OnCommand`.
    fn should_run_command(&self, _command: &str) -> bool {
        false
    }

    /// Execute a command hook. Purely observational (post-execution).
    fn run_command<'a>(
        &'a self,
        _input: CommandHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>> {
        Box::pin(async { Vec::new() })
    }
}

/// Registry of agent lifecycle hooks, populated at startup.
pub(crate) struct HookRegistry {
    hooks: Vec<Box<dyn AgentHook>>,
}

impl HookRegistry {
    pub(crate) fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    pub(crate) fn register(&mut self, hook: Box<dyn AgentHook>) {
        self.hooks.push(hook);
    }

    fn hook(&self, index: usize) -> Option<&dyn AgentHook> {
        self.hooks.get(index).map(|h| h.as_ref())
    }

    fn len(&self) -> usize {
        self.hooks.len()
    }
}

const AUTO_COMPRESS_THRESHOLD_PERCENT: usize = 90;
const AUTO_COMPRESS_KEEP_RECENT_TURNS: usize = 8;
const AUTO_COMPRESS_INPUT_CHAR_LIMIT: usize = 60_000;
const AUTO_COMPRESS_SUMMARY_CHAR_LIMIT: usize = 12_000;

pub(crate) fn find_auto_compress_cutoff(
    messages: &[ChatMessage],
    keep_recent_turns: usize,
) -> Option<usize> {
    if messages.len() <= 2 {
        return None;
    }

    let mut turn_starts = Vec::new();
    let mut idx = 1;
    while idx < messages.len() {
        turn_starts.push(idx);
        idx += turn_len(messages, idx);
    }

    if turn_starts.len() <= keep_recent_turns {
        return None;
    }

    let keep_from = turn_starts[turn_starts.len() - keep_recent_turns];
    (keep_from > 1).then_some(keep_from)
}

/// Extract the existing auto-generated summary from messages, if any.
fn extract_existing_summary(messages: &[ChatMessage]) -> Option<&str> {
    messages.iter().find_map(|msg| {
        msg.content
            .as_deref()
            .filter(|c| c.starts_with("## Context Summary (auto-generated)"))
    })
}

pub(crate) fn build_compression_source_text(messages: &[ChatMessage]) -> String {
    let mut lines = Vec::new();
    for msg in messages {
        match msg.role.as_str() {
            "user" => {
                if let Some(content) = msg.content.as_deref()
                    && !content.is_empty()
                {
                    lines.push(format!("User: {content}"));
                }
                // Record image attachments so the summary preserves the fact
                // that images were part of the conversation context.
                if let Some(images) = msg.images.as_ref()
                    && !images.is_empty()
                {
                    lines.push(format!("User attached {} image(s)", images.len()));
                }
            }
            "assistant" => {
                if let Some(content) = msg.content.as_deref()
                    && !content.is_empty()
                    // Skip previous auto-generated compression summaries so
                    // repeated compressions don't produce summary-of-summary drift.
                    && !content.starts_with("## Context Summary (auto-generated)")
                {
                    lines.push(format!("Assistant: {content}"));
                }
                if let Some(tool_calls) = msg.tool_calls.as_ref() {
                    for tc in tool_calls {
                        lines.push(format!(
                            "Assistant tool call [{}]: {} {}",
                            tc.id,
                            tc.function.name,
                            truncate(&tc.function.arguments, 1_500)
                        ));
                    }
                }
            }
            "tool" => {
                if let Some(content) = msg.content.as_deref() {
                    lines.push(format!(
                        "Tool result [{}]: {}",
                        msg.tool_call_id.as_deref().unwrap_or(""),
                        truncate(content, 4_000)
                    ));
                }
            }
            _ => {}
        }
    }
    truncate(&lines.join("\n"), AUTO_COMPRESS_INPUT_CHAR_LIMIT)
}

pub(crate) fn build_auto_summary_message(summary: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".into(),
        content: Some(format!(
            "## Context Summary (auto-generated)\n{}",
            truncate(summary.trim(), AUTO_COMPRESS_SUMMARY_CHAR_LIMIT)
        )),
        images: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(crate::now_epoch()),
    }
}

pub(crate) fn build_compressed_messages(
    messages: &[ChatMessage],
    compress_end: usize,
    summary: &str,
) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(messages.len() - compress_end + 2);
    out.push(messages[0].clone());
    out.push(build_auto_summary_message(summary));
    out.extend(messages[compress_end..].iter().cloned());
    out
}

pub(crate) fn build_context_compressed_event(
    removed_messages: usize,
    before_estimate: usize,
    after_estimate: usize,
    summary_tokens: usize,
    was_incremental: bool,
) -> serde_json::Value {
    let compression_ratio = if before_estimate > 0 {
        ((after_estimate as f64) / (before_estimate as f64) * 100.0).round() as usize
    } else {
        100
    };
    json!({
        "type": "context_compressed",
        "messages_removed": removed_messages,
        "before_estimate": before_estimate,
        "after_estimate": after_estimate,
        "summary_tokens": summary_tokens,
        "compression_ratio": compression_ratio,
        "incremental": was_incremental,
    })
}

pub(crate) struct AutoCompressContextHook {
    threshold_percent: usize,
    keep_recent_turns: usize,
}

impl AutoCompressContextHook {
    pub(crate) fn new() -> Self {
        Self {
            threshold_percent: AUTO_COMPRESS_THRESHOLD_PERCENT,
            keep_recent_turns: AUTO_COMPRESS_KEEP_RECENT_TURNS,
        }
    }
}

impl AgentHook for AutoCompressContextHook {
    fn name(&self) -> &'static str {
        "auto_compress_context"
    }

    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeAnalyze
    }

    fn should_run(
        &self,
        messages: &[ChatMessage],
        provider: Provider,
        input_budget: usize,
        _cycle: usize,
    ) -> bool {
        if input_budget == 0 {
            return false;
        }
        if find_auto_compress_cutoff(messages, self.keep_recent_turns).is_none() {
            return false;
        }
        estimate_tokens_for_provider(provider, messages).saturating_mul(100)
            >= input_budget.saturating_mul(self.threshold_percent)
    }

    fn run<'a>(
        &'a self,
        input: HookInput,
        config: &'a Config,
        http: &'a Client,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async move {
            let Some(compress_end) =
                find_auto_compress_cutoff(&input.messages, self.keep_recent_turns)
            else {
                return HookOutput::NoOp;
            };

            let before_estimate = estimate_tokens_for_provider(input.provider, &input.messages);
            let to_compress = &input.messages[1..compress_end];
            let source_text = build_compression_source_text(to_compress);
            if source_text.trim().is_empty() {
                return HookOutput::NoOp;
            }

            // Incremental compression: if there's an existing summary in the
            // messages being compressed, provide it as prior context so the LLM
            // merges rather than starting from scratch.
            let existing_summary = extract_existing_summary(to_compress);
            let user_content = if let Some(prev) = existing_summary {
                format!(
                    "## Previous Summary\n{prev}\n\n## New Conversation To Merge\n{source_text}"
                )
            } else {
                source_text.clone()
            };

            let system_content = if existing_summary.is_some() {
                "You compress older conversation context for an AI coding assistant so the agent can keep working without losing actionable state.\n\nYou are given a previous summary and new conversation turns. Merge them into a single updated summary, preserving all still-relevant information from the previous summary and incorporating new details.\n\nProduce a single concise markdown summary with the following sections, in this order, omitting any section that has no content:\n- User goal & constraints: the user's current objective and any hard requirements, scope limits, language, or style rules they set.\n- Files & components touched: repository paths, functions, modules, or external systems involved. Preserve exact paths when mentioned.\n- Key findings: important facts surfaced by tools (search/read/run output) that the agent will need later. Prefer precise quotes or exact identifiers over paraphrase.\n- Decisions & rationale: design choices already made and why, including any approaches the user explicitly approved or rejected.\n- Failed attempts & blockers: what has been tried, why it failed, and any errors/warnings that remain unresolved.\n- Open issues / next steps: work still pending or the immediate next action the agent was about to take.\n\nHard rules:\n- Keep it factual and compact. No filler, no meta commentary (\"In summary...\"), no apologies.\n- Do not fabricate information that is not in the source text.\n- When merging, drop obsolete information from the previous summary that has been superseded by new conversation.\n- Preserve tool call IDs, error codes, and exact identifiers verbatim when they appear.\n- Do not wrap the output in code blocks.\n- Match the language of the source conversation."
            } else {
                "You compress older conversation context for an AI coding assistant so the agent can keep working without losing actionable state.\n\nProduce a single concise markdown summary with the following sections, in this order, omitting any section that has no content:\n- User goal & constraints: the user's current objective and any hard requirements, scope limits, language, or style rules they set.\n- Files & components touched: repository paths, functions, modules, or external systems involved. Preserve exact paths when mentioned.\n- Key findings: important facts surfaced by tools (search/read/run output) that the agent will need later. Prefer precise quotes or exact identifiers over paraphrase.\n- Decisions & rationale: design choices already made and why, including any approaches the user explicitly approved or rejected.\n- Failed attempts & blockers: what has been tried, why it failed, and any errors/warnings that remain unresolved.\n- Open issues / next steps: work still pending or the immediate next action the agent was about to take.\n\nHard rules:\n- Keep it factual and compact. No filler, no meta commentary (\"In summary...\"), no apologies.\n- Do not fabricate information that is not in the source text.\n- Preserve tool call IDs, error codes, and exact identifiers verbatim when they appear.\n- Do not wrap the output in code blocks.\n- Match the language of the source conversation."
            };

            let prompt = vec![
                ChatMessage {
                    role: "system".into(),
                    content: Some(system_content.into()),
                    images: None,
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: None,
                },
                ChatMessage {
                    role: "user".into(),
                    content: Some(user_content),
                    images: None,
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: Some(crate::now_epoch()),
                },
            ];

            // Use context_model → primary fallback chain for compression.
            let compress_model = config.context_model_or(&input.model);
            let resolved = config.resolve_model(compress_model);
            let summary = match providers::call_llm_simple_with_usage(
                http,
                &resolved,
                &prompt,
                &input.workspace,
                config.s3.as_ref(),
                config.max_llm_retries,
            )
            .await
            {
                Ok(summary) if !summary.content.trim().is_empty() => summary,
                Ok(_) => return HookOutput::NoOp,
                Err(e) => {
                    // Emit failure event so the frontend can inform the user.
                    return HookOutput::ReplaceMessages {
                        messages: input.messages,
                        events: vec![json!({
                            "type": "context_compress_failed",
                            "error": e.to_string(),
                        })],
                        usage: None,
                    };
                }
            };

            let summary_text = summary.content;
            let messages = build_compressed_messages(&input.messages, compress_end, &summary_text);
            let after_estimate = estimate_tokens_for_provider(input.provider, &messages);
            let removed_messages = compress_end.saturating_sub(1);
            let summary_tokens = estimate_tokens_for_provider(input.provider, &messages[1..2]);
            let provider_name = config.resolve_provider_name(compress_model);
            let input_tokens = summary
                .input_tokens
                .unwrap_or_else(|| estimate_tokens_for_provider(resolved.provider, &prompt) as u64);
            let output_tokens = summary.output_tokens.unwrap_or_else(|| {
                estimate_tokens_for_provider(resolved.provider, &messages[1..2]) as u64
            });

            HookOutput::ReplaceMessages {
                messages,
                events: vec![build_context_compressed_event(
                    removed_messages,
                    before_estimate,
                    after_estimate,
                    summary_tokens,
                    existing_summary.is_some(),
                )],
                usage: Some(UsageUpdate {
                    input_tokens,
                    output_tokens,
                    input_source: if summary.input_tokens.is_some() {
                        "provider".to_string()
                    } else {
                        "estimated".to_string()
                    },
                    output_source: if summary.output_tokens.is_some() {
                        "provider".to_string()
                    } else {
                        "estimated".to_string()
                    },
                    labels: build_usage_labels(
                        input_tokens,
                        output_tokens,
                        Some(&provider_name),
                        Some(USAGE_ROLE_CONTEXT),
                    ),
                }),
            }
        })
    }
}

/// Run all hooks registered at the given point for the specified session.
///
/// Handles the lock → check → unlock → run → relock → apply pattern:
///   1. Lock session, call `should_run` for each hook at this point.
///   2. Drop lock, call `run` for each eligible hook (safe for async I/O).
///   3. Re-lock, apply any `ReplaceMessages` mutations.
///
/// Returns how many hooks actually fired.
pub(crate) async fn run_hooks(
    registry: &HookRegistry,
    point: agent::HookPoint,
    sessions: &Mutex<HashMap<String, Session>>,
    session_id: &str,
    config: &Config,
    http: &Client,
    cycle: usize,
) -> Vec<serde_json::Value> {
    let mut events = Vec::new();
    for index in 0..registry.len() {
        let hook = match registry.hook(index) {
            Some(h) => h,
            None => continue,
        };
        if hook.point() != point {
            continue;
        }

        let should_run = {
            let sessions_guard = sessions.lock().await;
            let session = match sessions_guard.get(session_id) {
                Some(s) => s,
                None => break,
            };
            let model = session.effective_model(&config.model);
            let provider = config.resolve_model(model).provider;
            let input_budget = context_input_budget_for_model(config, model);
            hook.should_run(&session.messages, provider, input_budget, cycle)
        };
        if !should_run {
            continue;
        }

        // Build owned input without lock.
        let input = {
            let sessions_guard = sessions.lock().await;
            let session = match sessions_guard.get(session_id) {
                Some(s) => s,
                None => break,
            };
            let model = session.effective_model(&config.model).to_string();
            let provider = config.resolve_model(&model).provider;
            let input_budget = context_input_budget_for_model(config, &model);
            HookInput {
                messages: session.messages.clone(),
                model,
                provider,
                workspace: session.workspace.clone(),
                input_budget,
                cycle,
            }
        }; // lock dropped

        let output = hook.run(input, config, http).await;

        match output {
            HookOutput::ReplaceMessages {
                messages: new_msgs,
                events: hook_events,
                usage,
            } => {
                let mut sessions_guard = sessions.lock().await;
                if let Some(session) = sessions_guard.get_mut(session_id) {
                    session.messages = new_msgs;
                    session.updated_at = crate::now_epoch();
                    if let Some(usage) = usage.as_ref() {
                        apply_usage_update(session, usage);
                    }
                }
                events.extend(hook_events);
            }
            HookOutput::NoOp
            | HookOutput::ModifyToolArgs { .. }
            | HookOutput::ModifyToolResult { .. }
            | HookOutput::Reject { .. }
            | HookOutput::ModifyLlmParams { .. } => {}
        }
    }
    events
}

// ── Tool-level hook dispatch ─────────────────────────────────────────────────

/// Run tool-level hooks (`BeforeToolExec` / `AfterToolExec`) across the registry.
///
/// Short-circuits on `Reject`. Folds `ModifyToolArgs` sequentially for
/// `BeforeToolExec`, or `ModifyToolResult` for `AfterToolExec`.
/// Returns the final `HookOutput` after all eligible hooks have run.
pub(crate) async fn run_tool_hooks(
    registry: &HookRegistry,
    point: agent::HookPoint,
    mut input: ToolHookInput,
    config: &Config,
) -> HookOutput {
    let mut modified = false;
    for index in 0..registry.len() {
        let hook = match registry.hook(index) {
            Some(h) => h,
            None => continue,
        };
        if hook.point() != point {
            continue;
        }
        if !hook.should_run_tool(&input.tool_name, point) {
            continue;
        }

        // Snapshot invariant fields before consuming input.
        let tool_name = input.tool_name.clone();
        let tool_id = input.tool_id.clone();
        let cycle = input.cycle;
        let workspace = input.workspace.clone();
        let outcome_output = input.outcome_output.clone();
        let outcome_is_error = input.outcome_is_error;
        let outcome_duration_ms = input.outcome_duration_ms;
        let tool_args = input.tool_args.clone();

        let output = hook.run_tool(input, config).await;
        match output {
            HookOutput::Reject { .. } if point == agent::HookPoint::BeforeToolExec => {
                return output;
            }
            HookOutput::ModifyToolArgs { args } if point == agent::HookPoint::BeforeToolExec => {
                modified = true;
                input = ToolHookInput {
                    tool_name,
                    tool_args: args,
                    tool_id,
                    cycle,
                    workspace,
                    outcome_output,
                    outcome_is_error,
                    outcome_duration_ms,
                };
            }
            HookOutput::ModifyToolResult { result } if point == agent::HookPoint::AfterToolExec => {
                modified = true;
                input = ToolHookInput {
                    tool_name,
                    tool_args,
                    tool_id,
                    cycle,
                    workspace,
                    outcome_output: Some(result),
                    outcome_is_error,
                    outcome_duration_ms,
                };
            }
            HookOutput::NoOp => {
                input = ToolHookInput {
                    tool_name,
                    tool_args,
                    tool_id,
                    cycle,
                    workspace,
                    outcome_output,
                    outcome_is_error,
                    outcome_duration_ms,
                };
            }
            _invalid => {
                eprintln!(
                    "[hooks] warning: hook {} returned output type invalid for {:?}, treating as NoOp",
                    hook.name(),
                    point
                );
                input = ToolHookInput {
                    tool_name,
                    tool_args,
                    tool_id,
                    cycle,
                    workspace,
                    outcome_output,
                    outcome_is_error,
                    outcome_duration_ms,
                };
            }
        }
    }
    if !modified {
        return HookOutput::NoOp;
    }
    if point == agent::HookPoint::BeforeToolExec {
        HookOutput::ModifyToolArgs {
            args: input.tool_args,
        }
    } else if let Some(result) = input.outcome_output {
        HookOutput::ModifyToolResult { result }
    } else {
        HookOutput::NoOp
    }
}

// ── LLM-call-level hook dispatch ─────────────────────────────────────────────

/// Run LLM-call-level hooks (`BeforeLlmCall`) across the registry.
///
/// Folds `ModifyLlmParams` sequentially: `extra_system` strings are appended,
/// the last `think_override` wins.
/// Returns the accumulated `HookOutput`.
pub(crate) async fn run_llm_hooks(
    registry: &HookRegistry,
    input: &LlmHookInput,
    config: &Config,
) -> HookOutput {
    let mut extra_system: Option<String> = None;
    let mut think_override: Option<String> = None;
    let mut any_modified = false;
    let mut running_messages = input.messages.clone();

    for index in 0..registry.len() {
        let hook = match registry.hook(index) {
            Some(h) => h,
            None => continue,
        };
        if hook.point() != agent::HookPoint::BeforeLlmCall {
            continue;
        }
        if !hook.should_run_llm(input.cycle) {
            continue;
        }

        let snapshot = LlmHookInput {
            messages: running_messages.clone(),
            model: input.model.clone(),
            think_level: think_override
                .as_deref()
                .unwrap_or(&input.think_level)
                .to_string(),
            cycle: input.cycle,
            tool_count: input.tool_count,
        };
        let output = hook.run_llm(snapshot, config).await;
        if let HookOutput::ModifyLlmParams {
            extra_system: es,
            think_override: to,
        } = output
        {
            if let Some(s) = es {
                // Update running messages so subsequent hooks see the injection.
                if let Some(first) = running_messages.first_mut()
                    && first.role == "system"
                    && let Some(content) = first.content.as_mut()
                {
                    content.push('\n');
                    content.push_str(&s);
                }
                extra_system = Some(match extra_system {
                    Some(existing) => format!("{existing}\n{s}"),
                    None => s,
                });
                any_modified = true;
            }
            if let Some(t) = to {
                think_override = Some(t);
                any_modified = true;
            }
        }
    }
    if any_modified {
        HookOutput::ModifyLlmParams {
            extra_system,
            think_override,
        }
    } else {
        HookOutput::NoOp
    }
}

// ── Command hook dispatch ────────────────────────────────────────────────────

/// Run post-command observation hooks (`OnCommand`) across the registry.
///
/// Returns accumulated frontend events from all hooks (purely observational).
pub(crate) async fn run_command_hooks(
    registry: &HookRegistry,
    input: &CommandHookInput,
    config: &Config,
) -> Vec<serde_json::Value> {
    let mut events = Vec::new();
    for index in 0..registry.len() {
        let hook = match registry.hook(index) {
            Some(h) => h,
            None => continue,
        };
        if hook.point() != agent::HookPoint::OnCommand {
            continue;
        }
        if !hook.should_run_command(&input.command) {
            continue;
        }
        let snapshot = CommandHookInput {
            command: input.command.clone(),
            args: input.args.clone(),
            result_type: input.result_type.clone(),
            session_id: input.session_id.clone(),
        };
        let hook_events = hook.run_command(snapshot, config).await;
        events.extend(hook_events);
    }
    events
}

#[cfg(test)]
#[path = "tests/hooks_tests.rs"]
mod hooks_tests;
