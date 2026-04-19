use std::collections::HashMap;

use crate::{ChatMessage, Session, config::Config, config::Provider, prompts};

pub(crate) const USAGE_ROLE_PRIMARY: &str = "Primary";
pub(crate) const USAGE_ROLE_FAST: &str = "Fast";
pub(crate) const USAGE_ROLE_SUB_AGENT: &str = "Sub-Agent";
pub(crate) const USAGE_ROLE_MEMORY: &str = "Memory";
pub(crate) const USAGE_ROLE_REFLECTION: &str = "Reflection";
pub(crate) const USAGE_ROLE_CONTEXT: &str = "Context";

const USAGE_PROVIDER_PREFIX: &str = "provider:";
const USAGE_ROLE_PREFIX: &str = "role:";

#[derive(Clone, Debug, Default)]
pub(crate) struct UsageUpdate {
    pub(crate) input_tokens: u64,
    pub(crate) output_tokens: u64,
    pub(crate) input_source: String,
    pub(crate) output_source: String,
    pub(crate) labels: HashMap<String, [u64; 2]>,
}

pub(crate) fn usage_provider_label(label: &str) -> String {
    format!("{USAGE_PROVIDER_PREFIX}{label}")
}

pub(crate) fn usage_role_label(label: &str) -> String {
    format!("{USAGE_ROLE_PREFIX}{label}")
}

pub(crate) fn build_usage_labels(
    input_tokens: u64,
    output_tokens: u64,
    provider_label: Option<&str>,
    role_label: Option<&str>,
) -> HashMap<String, [u64; 2]> {
    let mut labels = HashMap::new();
    if let Some(label) = provider_label.filter(|label| !label.is_empty()) {
        labels.insert(usage_provider_label(label), [input_tokens, output_tokens]);
    }
    if let Some(label) = role_label.filter(|label| !label.is_empty()) {
        labels.insert(usage_role_label(label), [input_tokens, output_tokens]);
    }
    labels
}

pub(crate) fn split_usage_labels(
    labels: &HashMap<String, [u64; 2]>,
) -> (HashMap<String, [u64; 2]>, HashMap<String, [u64; 2]>) {
    let mut providers = HashMap::new();
    let mut roles = HashMap::new();
    for (label, pair) in labels {
        if let Some(name) = label.strip_prefix(USAGE_PROVIDER_PREFIX) {
            providers.insert(name.to_string(), *pair);
        } else if let Some(name) = label.strip_prefix(USAGE_ROLE_PREFIX) {
            roles.insert(name.to_string(), *pair);
        } else {
            // Backward compatibility: old snapshots stored raw provider names.
            providers.insert(label.clone(), *pair);
        }
    }
    (providers, roles)
}

fn merge_usage_labels(into: &mut HashMap<String, [u64; 2]>, labels: &HashMap<String, [u64; 2]>) {
    for (label, [input_tokens, output_tokens]) in labels {
        let entry = into.entry(label.clone()).or_insert([0, 0]);
        entry[0] = entry[0].saturating_add(*input_tokens);
        entry[1] = entry[1].saturating_add(*output_tokens);
    }
}

pub(crate) fn apply_usage_update(session: &mut Session, update: &UsageUpdate) {
    rollover_daily_usage_if_needed(session);
    session.input_tokens = session.input_tokens.saturating_add(update.input_tokens);
    session.output_tokens = session.output_tokens.saturating_add(update.output_tokens);
    session.daily_input_tokens = session
        .daily_input_tokens
        .saturating_add(update.input_tokens);
    session.daily_output_tokens = session
        .daily_output_tokens
        .saturating_add(update.output_tokens);
    session.input_token_source = update.input_source.clone();
    session.output_token_source = update.output_source.clone();
    merge_usage_labels(&mut session.daily_provider_usage, &update.labels);
    merge_usage_labels(&mut session.total_label_usage, &update.labels);
}

// ── Context Management ──────────────────────────────────────────────────────

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages.iter().map(message_token_len).sum()
}

const OPENAI_TOOL_CALL_OVERHEAD_TOKENS: usize = 8;
const OPENAI_TOOL_RESULT_OVERHEAD_TOKENS: usize = 6;
const ANTHROPIC_TOOL_USE_OVERHEAD_TOKENS: usize = 16;
const ANTHROPIC_TOOL_RESULT_OVERHEAD_TOKENS: usize = 14;
const OPENAI_MIN_REPLY_RESERVE_TOKENS: usize = 2_048;
const ANTHROPIC_MIN_REPLY_RESERVE_TOKENS: usize = 4_096;
const CONTEXT_REPLY_RESERVE_RATIO_DIVISOR: usize = 10;
const CONTEXT_REPLY_RESERVE_CAP_DIVISOR: usize = 5;
const REQUEST_STRUCTURAL_OVERHEAD_TOKENS: usize = 256;

fn anthropic_thinking_budget_tokens(level: &str) -> usize {
    match level {
        "minimal" => 1_024,
        "low" => 4_096,
        "medium" => 10_240,
        "high" => 16_384,
        "xhigh" => 32_768,
        _ => 10_240,
    }
}

pub(crate) fn message_token_len_for_provider(provider: Provider, message: &ChatMessage) -> usize {
    let base = message_token_len(message);
    match provider {
        Provider::OpenAI | Provider::Ollama => {
            let tool_call_overhead = message
                .tool_calls
                .as_ref()
                .map(|calls| calls.len() * OPENAI_TOOL_CALL_OVERHEAD_TOKENS)
                .unwrap_or(0);
            let tool_result_overhead = if message.role == "tool" {
                OPENAI_TOOL_RESULT_OVERHEAD_TOKENS
            } else {
                0
            };
            base + tool_call_overhead + tool_result_overhead
        }
        Provider::Anthropic => {
            let tool_use_overhead = message
                .tool_calls
                .as_ref()
                .map(|calls| calls.len() * ANTHROPIC_TOOL_USE_OVERHEAD_TOKENS)
                .unwrap_or(0);
            let tool_result_overhead = if message.role == "tool" {
                ANTHROPIC_TOOL_RESULT_OVERHEAD_TOKENS
            } else {
                0
            };
            base + tool_use_overhead + tool_result_overhead
        }
    }
}

pub(crate) fn estimate_tokens_for_provider(provider: Provider, messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|message| message_token_len_for_provider(provider, message))
        .sum()
}

pub(crate) fn context_input_budget_for_model(config: &Config, model_ref: &str) -> usize {
    let ctx_limit = config.context_limit_for_model(model_ref);
    let resolved = config.resolve_model(model_ref);
    let provider_floor = match resolved.provider {
        Provider::OpenAI | Provider::Ollama => OPENAI_MIN_REPLY_RESERVE_TOKENS,
        Provider::Anthropic => ANTHROPIC_MIN_REPLY_RESERVE_TOKENS,
    };
    let ratio_reserve = ctx_limit / CONTEXT_REPLY_RESERVE_RATIO_DIVISOR;
    let model_reserve = resolved
        .max_tokens
        .map(|value| value as usize)
        .unwrap_or(provider_floor)
        .min(ctx_limit / CONTEXT_REPLY_RESERVE_CAP_DIVISOR);
    let reserve = provider_floor.max(ratio_reserve).max(model_reserve);
    let minimum_budget = ctx_limit.min(1_024);
    ctx_limit.saturating_sub(reserve).max(minimum_budget)
}

pub(crate) fn context_input_budget_for_runtime(
    config: &Config,
    model_ref: &str,
    think_level: &str,
) -> usize {
    let ctx_limit = config.context_limit_for_model(model_ref);
    let resolved = config.resolve_model(model_ref);
    let provider_floor = match resolved.provider {
        Provider::OpenAI | Provider::Ollama => OPENAI_MIN_REPLY_RESERVE_TOKENS,
        Provider::Anthropic => ANTHROPIC_MIN_REPLY_RESERVE_TOKENS,
    };
    let ratio_reserve = ctx_limit / CONTEXT_REPLY_RESERVE_RATIO_DIVISOR;
    let mut model_reserve = resolved
        .max_tokens
        .map(|value| value as usize)
        .unwrap_or(provider_floor)
        .min(ctx_limit / CONTEXT_REPLY_RESERVE_CAP_DIVISOR);

    if resolved.provider == Provider::Anthropic && think_level != "off" {
        model_reserve = model_reserve.saturating_add(anthropic_thinking_budget_tokens(think_level));
    }

    let reserve = provider_floor.max(ratio_reserve).max(model_reserve);
    let minimum_budget = ctx_limit.min(1_024);
    ctx_limit.saturating_sub(reserve).max(minimum_budget)
}

pub(crate) fn estimate_json_value_tokens(value: &serde_json::Value) -> usize {
    serde_json::to_string(value)
        .map(|text| text.len().div_ceil(4))
        .unwrap_or(0)
}

pub(crate) fn estimate_extra_tools_tokens(extra_tools: &[serde_json::Value]) -> usize {
    extra_tools.iter().map(estimate_json_value_tokens).sum()
}

fn builtin_tool_definitions_for_provider(provider: Provider) -> Vec<serde_json::Value> {
    match provider {
        Provider::OpenAI => {
            serde_json::from_value(crate::tools::tool_definitions_openai()).unwrap_or_default()
        }
        Provider::Ollama => {
            serde_json::from_value(crate::tools::tool_definitions_ollama()).unwrap_or_default()
        }
        Provider::Anthropic => {
            serde_json::from_value(crate::tools::tool_definitions_anthropic()).unwrap_or_default()
        }
    }
}

pub(crate) fn estimate_tool_schema_tokens_for_provider(
    provider: Provider,
    extra_tools: &[serde_json::Value],
) -> usize {
    let builtin_tools = builtin_tool_definitions_for_provider(provider);
    estimate_extra_tools_tokens(&builtin_tools)
        .saturating_add(estimate_extra_tools_tokens(extra_tools))
}

pub(crate) fn estimate_request_tokens_for_provider(
    provider: Provider,
    messages: &[ChatMessage],
    extra_tools: &[serde_json::Value],
) -> usize {
    estimate_tokens_for_provider(provider, messages)
        .saturating_add(estimate_tool_schema_tokens_for_provider(
            provider,
            extra_tools,
        ))
        .saturating_add(REQUEST_STRUCTURAL_OVERHEAD_TOKENS)
}

pub(crate) fn request_message_budget_for_runtime(
    config: &Config,
    model_ref: &str,
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> usize {
    let provider = config.resolve_model(model_ref).provider;
    context_input_budget_for_runtime(config, model_ref, think_level).saturating_sub(
        estimate_tool_schema_tokens_for_provider(provider, extra_tools)
            .saturating_add(REQUEST_STRUCTURAL_OVERHEAD_TOKENS),
    )
}

/// Compute message budget for a caller that already has the complete set of
/// tool definition JSON values it will send (e.g. sub-agents with a filtered
/// tool subset). Unlike `request_message_budget_for_runtime` this does NOT
/// re-derive builtin tools — it uses the provided `tool_defs` directly.
pub(crate) fn message_budget_for_tool_defs(
    config: &Config,
    model_ref: &str,
    think_level: &str,
    tool_defs: &[serde_json::Value],
) -> usize {
    context_input_budget_for_runtime(config, model_ref, think_level).saturating_sub(
        estimate_extra_tools_tokens(tool_defs).saturating_add(REQUEST_STRUCTURAL_OVERHEAD_TOKENS),
    )
}

pub(crate) fn format_token_count(value: u64) -> String {
    fn format_scaled(value: u64, divisor: u64, unit: &str) -> String {
        let scaled_tenths = (value * 10 + divisor / 2) / divisor;
        if scaled_tenths.is_multiple_of(10) {
            format!("{}{}", scaled_tenths / 10, unit)
        } else {
            format!("{}.{}{}", scaled_tenths / 10, scaled_tenths % 10, unit)
        }
    }

    if value >= 1_000_000 {
        format_scaled(value, 1_000_000, "M")
    } else if value >= 1_000 {
        format_scaled(value, 1_000, "K")
    } else {
        value.to_string()
    }
}

pub(crate) fn current_daily_token_usage(session: &Session) -> (u64, u64) {
    let today = prompts::current_local_snapshot().today();
    if session.token_usage_day == today {
        (session.daily_input_tokens, session.daily_output_tokens)
    } else {
        (0, 0)
    }
}

pub(crate) fn accumulate_daily_token_usage<'a>(
    sessions: impl IntoIterator<Item = &'a Session>,
) -> (u64, u64) {
    sessions.into_iter().map(current_daily_token_usage).fold(
        (0_u64, 0_u64),
        |(input_acc, output_acc), (input, output)| {
            (
                input_acc.saturating_add(input),
                output_acc.saturating_add(output),
            )
        },
    )
}

pub(crate) fn format_usage_block(
    label: &str,
    description: &str,
    input_tokens: u64,
    output_tokens: u64,
) -> String {
    format!(
        "{label}: # {description}\n\tinput_tokens: {}\n\toutput_tokens: {}\n\ttotal_tokens: {}",
        format_token_count(input_tokens),
        format_token_count(output_tokens),
        format_token_count(input_tokens.saturating_add(output_tokens)),
    )
}

pub(crate) fn rollover_daily_usage_if_needed(session: &mut Session) {
    let today = prompts::current_local_snapshot().today();
    if session.token_usage_day != today {
        // Snapshot the previous day before resetting.
        if (session.daily_input_tokens > 0 || session.daily_output_tokens > 0)
            && !session.token_usage_day.is_empty()
        {
            let snapshot = crate::DailyUsageSnapshot {
                date: session.token_usage_day.clone(),
                input: session.daily_input_tokens,
                output: session.daily_output_tokens,
                providers: session.daily_provider_usage.clone(),
            };
            session.usage_history.push(snapshot);
            if session.usage_history.len() > crate::USAGE_HISTORY_CAP {
                let excess = session.usage_history.len() - crate::USAGE_HISTORY_CAP;
                session.usage_history.drain(..excess);
            }
        }
        session.daily_input_tokens = 0;
        session.daily_output_tokens = 0;
        session.daily_provider_usage.clear();
        session.token_usage_day = today;
    }
}

pub(crate) fn update_session_token_usage_with_provider(
    session: &mut Session,
    input_tokens: u64,
    output_tokens: u64,
    input_source: &str,
    output_source: &str,
    provider_label: Option<&str>,
    role_label: Option<&str>,
) {
    update_session_token_usage_with_providers(
        session,
        input_tokens,
        output_tokens,
        input_source,
        output_source,
        &build_usage_labels(input_tokens, output_tokens, provider_label, role_label),
    );
}

pub(crate) fn update_session_token_usage_with_providers(
    session: &mut Session,
    input_tokens: u64,
    output_tokens: u64,
    input_source: &str,
    output_source: &str,
    provider_usage: &HashMap<String, [u64; 2]>,
) {
    apply_usage_update(
        session,
        &UsageUpdate {
            input_tokens,
            output_tokens,
            input_source: input_source.to_string(),
            output_source: output_source.to_string(),
            labels: provider_usage.clone(),
        },
    );
}

/// Estimate token count for a string with CJK awareness.
///
/// Latin/ASCII text averages ~4 bytes per token. CJK characters (Chinese,
/// Japanese Kanji, Korean) average ~1.5 characters per token in typical
/// tokenizers (cl100k, o200k). This function splits the estimation
/// accordingly instead of a flat `len / 4`.
fn estimate_text_tokens(text: &str) -> usize {
    let mut cjk_chars: usize = 0;
    let mut other_bytes: usize = 0;
    for c in text.chars() {
        if is_cjk_like(c) {
            cjk_chars += 1;
        } else {
            other_bytes += c.len_utf8();
        }
    }
    // CJK: ~1-2 tokens per character in typical tokenizers; use 1 token/char
    // as a conservative (slightly over-counting) estimate.
    // Other: ~4 bytes per token.
    let cjk_tokens = cjk_chars;
    let other_tokens = other_bytes / 4;
    cjk_tokens + other_tokens
}

/// Quick CJK character classifier for token estimation.
fn is_cjk_like(c: char) -> bool {
    matches!(
        c,
        '\u{4E00}'..='\u{9FFF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{3040}'..='\u{309F}'
            | '\u{30A0}'..='\u{30FF}'
            | '\u{AC00}'..='\u{D7AF}'
    )
}

pub(crate) fn message_token_len(message: &ChatMessage) -> usize {
    let content_tokens = message
        .content
        .as_ref()
        .map(|c| estimate_text_tokens(c))
        .unwrap_or(0);
    let tc_tokens = message
        .tool_calls
        .as_ref()
        .map(|tcs| {
            tcs.iter()
                .map(|tc| {
                    // Tool call JSON is typically ASCII, use simple /4.
                    (tc.function.name.len() + tc.function.arguments.len()) / 4
                })
                .sum::<usize>()
        })
        .unwrap_or(0);
    // Each image costs ~85 tokens (conservative; actual varies by resolution).
    let img_tokens = message
        .images
        .as_ref()
        .map(|imgs| imgs.len() * 85)
        .unwrap_or(0);
    content_tokens + tc_tokens + 3 + img_tokens
}

/// Measure the size of the conversational "turn" starting at `start`.
///
/// A turn is one of:
///   - user + optional assistant reply (+ optional tool results)
///   - assistant without tool_calls (1 message)
///   - orphaned assistant(tool_calls) + tool results (recovery case)
///
/// Returns how many messages belong to this turn.
pub(crate) fn turn_len(messages: &[ChatMessage], start: usize) -> usize {
    let msg = &messages[start];
    if msg.role == "user" {
        // Remove the user message together with its following assistant reply,
        // if present, so we prune complete conversational turns.
        if start + 1 < messages.len() {
            let next = &messages[start + 1];
            if next.role == "assistant" {
                if let Some(tcs) = &next.tool_calls
                    && !tcs.is_empty()
                {
                    let tool_results = messages[start + 2..]
                        .iter()
                        .take_while(|m| m.role == "tool")
                        .count();
                    return 2 + tool_results; // user + assistant + tool results
                }
                return 2; // user + assistant text reply
            }
        }
        return 1; // standalone user
    }
    if msg.role == "assistant"
        && let Some(tcs) = &msg.tool_calls
        && !tcs.is_empty()
    {
        let tool_results = messages[start + 1..]
            .iter()
            .take_while(|m| m.role == "tool")
            .count();
        return 1 + tool_results; // assistant + tool results
    }
    1 // standalone assistant or tool message
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn prune_messages(messages: &mut Vec<ChatMessage>, max_tokens: usize) {
    // Keep: system message (index 0) + as many recent messages as fit.
    // Remove oldest non-system messages in complete turns so we never
    // leave orphaned tool_calls or tool results.
    //
    // Pre-compute per-message costs in a single pass, then walk turns
    // using cached values. ONE final drain avoids repeated O(n) shifts.
    let costs: Vec<usize> = messages.iter().map(message_token_len).collect();
    let mut estimated: usize = costs.iter().sum();
    let mut total_remove = 0;
    let mut pos = 1;
    while estimated > max_tokens && messages.len() - total_remove > 2 && pos < messages.len() {
        let count = turn_len(messages, pos);
        let removed: usize = costs[pos..pos + count].iter().sum();
        total_remove += count;
        pos += count;
        estimated = estimated.saturating_sub(removed);
    }
    if total_remove > 0 {
        messages.drain(1..1 + total_remove);
    }
}

pub(crate) fn prune_messages_for_provider(
    messages: &mut Vec<ChatMessage>,
    provider: Provider,
    max_tokens: usize,
) {
    let costs: Vec<usize> = messages
        .iter()
        .map(|m| message_token_len_for_provider(provider, m))
        .collect();
    let mut estimated: usize = costs.iter().sum();
    let mut total_remove = 0;
    let mut pos = 1;
    while estimated > max_tokens && messages.len() - total_remove > 2 && pos < messages.len() {
        let count = turn_len(messages, pos);
        let removed: usize = costs[pos..pos + count].iter().sum();
        total_remove += count;
        pos += count;
        estimated = estimated.saturating_sub(removed);
    }
    if total_remove > 0 {
        messages.drain(1..1 + total_remove);
    }
}
