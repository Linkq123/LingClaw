use crate::{ChatMessage, Session, config::Config, config::Provider, prompts};

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

pub(crate) fn update_session_token_usage(
    session: &mut Session,
    input_tokens: u64,
    output_tokens: u64,
    input_source: &str,
    output_source: &str,
) {
    let today = prompts::current_local_snapshot().today();
    if session.token_usage_day != today {
        session.daily_input_tokens = 0;
        session.daily_output_tokens = 0;
        session.token_usage_day = today;
    }
    session.input_tokens = session.input_tokens.saturating_add(input_tokens);
    session.output_tokens = session.output_tokens.saturating_add(output_tokens);
    session.daily_input_tokens = session.daily_input_tokens.saturating_add(input_tokens);
    session.daily_output_tokens = session.daily_output_tokens.saturating_add(output_tokens);
    session.input_token_source = input_source.to_string();
    session.output_token_source = output_source.to_string();
}

pub(crate) fn message_token_len(message: &ChatMessage) -> usize {
    let content_len = message.content.as_ref().map(|c| c.len()).unwrap_or(0);
    let tc_len = message
        .tool_calls
        .as_ref()
        .map(|tcs| {
            tcs.iter()
                .map(|tc| tc.function.name.len() + tc.function.arguments.len())
                .sum::<usize>()
        })
        .unwrap_or(0);
    (content_len + tc_len + 10) / 4
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
    let mut estimated = estimate_tokens(messages);
    while estimated > max_tokens && messages.len() > 2 {
        let count = turn_len(messages, 1);
        let removed = messages[1..1 + count]
            .iter()
            .map(message_token_len)
            .sum::<usize>();
        messages.drain(1..1 + count);
        estimated = estimated.saturating_sub(removed);
    }
}

pub(crate) fn prune_messages_for_provider(
    messages: &mut Vec<ChatMessage>,
    provider: Provider,
    max_tokens: usize,
) {
    let mut estimated = estimate_tokens_for_provider(provider, messages);
    while estimated > max_tokens && messages.len() > 2 {
        let count = turn_len(messages, 1);
        let removed = messages[1..1 + count]
            .iter()
            .map(|message| message_token_len_for_provider(provider, message))
            .sum::<usize>();
        messages.drain(1..1 + count);
        estimated = estimated.saturating_sub(removed);
    }
}
