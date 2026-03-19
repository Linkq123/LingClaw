use std::{collections::HashMap, future::Future, path::PathBuf, pin::Pin};

use reqwest::Client;
use serde_json::json;
use tokio::sync::Mutex;

use crate::{
    agent,
    config::{Config, Provider},
    context::{context_input_budget_for_model, estimate_tokens_for_provider, turn_len},
    providers, truncate, ChatMessage, Session,
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
    },
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

pub(crate) fn build_compression_source_text(messages: &[ChatMessage]) -> String {
    let mut lines = Vec::new();
    for msg in messages {
        match msg.role.as_str() {
            "user" => {
                if let Some(content) = msg.content.as_deref() {
                    if !content.is_empty() {
                        lines.push(format!("User: {content}"));
                    }
                }
            }
            "assistant" => {
                if let Some(content) = msg.content.as_deref() {
                    if !content.is_empty() {
                        lines.push(format!("Assistant: {content}"));
                    }
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
) -> serde_json::Value {
    json!({
        "type": "context_compressed",
        "messages_removed": removed_messages,
        "before_estimate": before_estimate,
        "after_estimate": after_estimate,
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
            let source_text = build_compression_source_text(&input.messages[1..compress_end]);
            if source_text.trim().is_empty() {
                return HookOutput::NoOp;
            }

            let prompt = vec![
                ChatMessage {
                    role: "system".into(),
                    content: Some(
                        "You compress older conversation context for an AI coding assistant. Produce a concise markdown summary that preserves: user goal, important constraints, files or components touched, key tool findings, decisions made, failed attempts, and remaining open issues. Keep it factual and compact. Do not wrap in code blocks. Keep the same language as the source conversation."
                            .into(),
                    ),
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: None,
                },
                ChatMessage {
                    role: "user".into(),
                    content: Some(source_text),
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: Some(crate::now_epoch()),
                },
            ];

            let resolved = config.resolve_model(&input.model);
            let summary = match providers::call_llm_simple(http, &resolved, &prompt).await {
                Ok(summary) if !summary.trim().is_empty() => summary,
                _ => return HookOutput::NoOp,
            };

            let messages = build_compressed_messages(&input.messages, compress_end, &summary);
            let after_estimate = estimate_tokens_for_provider(input.provider, &messages);
            let removed_messages = compress_end.saturating_sub(1);

            HookOutput::ReplaceMessages {
                messages,
                events: vec![build_context_compressed_event(
                    removed_messages,
                    before_estimate,
                    after_estimate,
                )],
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
            } => {
                let mut sessions_guard = sessions.lock().await;
                if let Some(session) = sessions_guard.get_mut(session_id) {
                    session.messages = new_msgs;
                    session.updated_at = crate::now_epoch();
                }
                events.extend(hook_events);
            }
            HookOutput::NoOp => {}
        }
    }
    events
}
