use std::{
    collections::HashMap,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::{Stream, StreamExt};
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{ChatMessage, FunctionCall, LiveTx, Provider, ToolCall, live_send, tools};

// ══════════════════════════════════════════════════════════════════════════════
//  Provider Types
// ══════════════════════════════════════════════════════════════════════════════

pub(crate) struct ResolvedModel {
    pub(crate) provider: Provider,
    pub(crate) api_base: String,
    pub(crate) api_key: String,
    pub(crate) model_id: String,
    pub(crate) reasoning: bool,
    /// From model config `compat.thinkingFormat`: "qwen", "openai", "anthropic", "ollama", etc.
    pub(crate) thinking_format: Option<String>,
    /// From model config `maxTokens`.
    pub(crate) max_tokens: Option<u64>,
    /// Effective context window for the resolved model.
    pub(crate) context_window: u64,
    pub(crate) stream_include_usage: bool,
    pub(crate) anthropic_prompt_caching: bool,
}

pub(crate) struct LlmResponse {
    pub(crate) message: ChatMessage,
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
}

struct OpenAiStreamState {
    content_buf: String,
    tool_calls: Vec<ToolCall>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    client_gone: bool,
    reasoning_started: bool,
}

struct AnthropicStreamState {
    current_event_type: String,
    content_buf: String,
    tool_calls: Vec<ToolCall>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    block_tool_idx: HashMap<usize, usize>,
    client_gone: bool,
    reasoning_started: bool,
    thinking_block_idx: Option<usize>,
}

#[derive(Deserialize, Debug)]
struct OllamaStreamChunk {
    message: Option<OllamaMessage>,
    done: Option<bool>,
    prompt_eval_count: Option<u64>,
    eval_count: Option<u64>,
}

#[derive(Deserialize, Debug)]
struct OllamaMessage {
    content: Option<String>,
    thinking: Option<String>,
    tool_calls: Option<Vec<OllamaToolCall>>,
}

#[derive(Deserialize, Debug)]
struct OllamaToolCall {
    id: Option<String>,
    function: Option<OllamaFunction>,
}

#[derive(Deserialize, Debug)]
struct OllamaFunction {
    name: Option<String>,
    arguments: Option<Value>,
}

// ══════════════════════════════════════════════════════════════════════════════
//  OpenAI SSE Stream Models
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Deserialize, Debug)]
struct DeltaToolCall {
    index: Option<usize>,
    id: Option<String>,
    function: Option<DeltaFunction>,
}

#[derive(Deserialize, Debug)]
struct DeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize, Debug)]
struct StreamChoice {
    delta: StreamDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct StreamDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<DeltaToolCall>>,
}

#[derive(Deserialize, Debug)]
struct StreamChunk {
    choices: Option<Vec<StreamChoice>>,
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize, Debug)]
struct OpenAiUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

// ══════════════════════════════════════════════════════════════════════════════
//  Anthropic SSE Stream Models
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Deserialize, Debug)]
struct AnthropicEvent {
    index: Option<usize>,
    delta: Option<AnthropicDelta>,
    content_block: Option<AnthropicContentBlock>,
    message: Option<AnthropicMessage>,
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize, Debug)]
struct AnthropicMessage {
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize, Debug, Clone)]
struct AnthropicUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

#[derive(Deserialize, Debug)]
struct AnthropicDelta {
    #[serde(rename = "type")]
    delta_type: Option<String>,
    text: Option<String>,
    thinking: Option<String>,
    partial_json: Option<String>,
}

#[derive(Deserialize, Debug)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    id: Option<String>,
    name: Option<String>,
}

// ══════════════════════════════════════════════════════════════════════════════
//  Message Conversion
// ══════════════════════════════════════════════════════════════════════════════

/// Convert internal messages to clean OpenAI API format (strips timestamps and
/// extra fields so the provider receives only role/content/tool_calls/tool_call_id).
fn convert_messages_to_openai(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" | "user" => {
                out.push(json!({
                    "role": msg.role,
                    "content": msg.content.as_deref().unwrap_or(""),
                }));
            }
            "assistant" => {
                let mut item = json!({
                    "role": "assistant",
                    "content": msg.content.as_deref().unwrap_or(""),
                });
                if let Some(tool_calls) = &msg.tool_calls {
                    item["tool_calls"] = json!(tool_calls);
                }
                out.push(item);
            }
            "tool" => {
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": msg.tool_call_id.as_deref().unwrap_or(""),
                    "content": msg.content.as_deref().unwrap_or(""),
                }));
            }
            _ => {}
        }
    }

    out
}

/// Convert internal messages to Anthropic API format.
/// Returns (system_prompt, messages_array).
fn convert_messages_to_anthropic(messages: &[ChatMessage]) -> (String, Vec<serde_json::Value>) {
    let mut system = String::new();
    let mut out: Vec<serde_json::Value> = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => {
                system = msg.content.clone().unwrap_or_default();
            }
            "user" => {
                out.push(json!({
                    "role": "user",
                    "content": msg.content.as_deref().unwrap_or(""),
                }));
            }
            "assistant" => {
                let mut content_blocks: Vec<serde_json::Value> = Vec::new();
                if let Some(text) = &msg.content
                    && !text.is_empty()
                {
                    content_blocks.push(json!({"type": "text", "text": text}));
                }
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        let input: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or_else(|e| {
                                eprintln!(
                                    "warn: failed to parse tool arguments for {}: {e}",
                                    tc.function.name
                                );
                                json!({})
                            });
                        content_blocks.push(json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": tc.function.name,
                            "input": input,
                        }));
                    }
                }
                if content_blocks.is_empty() {
                    content_blocks.push(json!({"type": "text", "text": ""}));
                }
                out.push(json!({
                    "role": "assistant",
                    "content": content_blocks,
                }));
            }
            "tool" => {
                let tool_call_id = msg.tool_call_id.as_deref().unwrap_or("");
                let result_text = msg.content.as_deref().unwrap_or("");
                // Anthropic requires tool_result in a user message
                out.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": result_text,
                    }],
                }));
            }
            _ => {}
        }
    }
    (system, out)
}

fn convert_messages_to_ollama(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut tool_names_by_id: HashMap<String, String> = HashMap::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" | "user" => {
                out.push(json!({
                    "role": msg.role,
                    "content": msg.content.as_deref().unwrap_or(""),
                }));
            }
            "assistant" => {
                let mut item = json!({
                    "role": "assistant",
                    "content": msg.content.as_deref().unwrap_or(""),
                });
                if let Some(tool_calls) = &msg.tool_calls {
                    let calls = tool_calls
                        .iter()
                        .enumerate()
                        .map(|(idx, tool_call)| {
                            if !tool_call.id.is_empty() {
                                tool_names_by_id
                                    .insert(tool_call.id.clone(), tool_call.function.name.clone());
                            }
                            let arguments =
                                serde_json::from_str::<Value>(&tool_call.function.arguments)
                                    .unwrap_or_else(|_| json!(tool_call.function.arguments));
                            json!({
                                "type": "function",
                                "id": tool_call.id,
                                "function": {
                                    "index": idx,
                                    "name": tool_call.function.name,
                                    "arguments": arguments,
                                }
                            })
                        })
                        .collect::<Vec<_>>();
                    item["tool_calls"] = json!(calls);
                }
                out.push(item);
            }
            "tool" => {
                let mut item = json!({
                    "role": "tool",
                    "content": msg.content.as_deref().unwrap_or(""),
                });
                if let Some(tool_name) = msg
                    .tool_call_id
                    .as_ref()
                    .and_then(|tool_call_id| tool_names_by_id.get(tool_call_id))
                {
                    item["tool_name"] = json!(tool_name);
                }
                out.push(item);
            }
            _ => {}
        }
    }

    out
}

fn with_optional_bearer_auth(request: RequestBuilder, api_key: &str) -> RequestBuilder {
    if api_key.is_empty() {
        request
    } else {
        request.bearer_auth(api_key)
    }
}

fn ollama_request_options(resolved: &ResolvedModel) -> serde_json::Value {
    let mut options = serde_json::Map::new();
    options.insert("num_ctx".to_string(), json!(resolved.context_window));
    if let Some(max_tokens) = resolved.max_tokens {
        options.insert("num_predict".to_string(), json!(max_tokens));
    }
    serde_json::Value::Object(options)
}

// ══════════════════════════════════════════════════════════════════════════════
//  LLM Streaming Client
// ══════════════════════════════════════════════════════════════════════════════

/// Non-streaming LLM call — returns plain text. Used for conversation compression.
pub(crate) async fn call_llm_simple(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
) -> Result<String, String> {
    match resolved.provider {
        Provider::OpenAI => {
            let url = format!("{}/chat/completions", resolved.api_base);
            let api_messages = convert_messages_to_openai(messages);
            let mut body = json!({
                "model": resolved.model_id,
                "messages": api_messages,
            });
            if let Some(mt) = resolved.max_tokens {
                body["max_tokens"] = json!(mt);
            }
            let resp = http
                .post(&url)
                .bearer_auth(&resolved.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("HTTP error: {e}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(format!("API {status}: {text}"));
            }
            let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            Ok(data["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string())
        }
        Provider::Anthropic => {
            let url = format!("{}/v1/messages", resolved.api_base);
            let (system, msgs) = convert_messages_to_anthropic(messages);
            let max_tokens = resolved.max_tokens.unwrap_or(4096);
            let cache_enabled = anthropic_prompt_caching_enabled(resolved);
            let mut body = json!({
                "model": resolved.model_id,
                "messages": msgs,
                "max_tokens": max_tokens,
            });
            if !system.is_empty() {
                body["system"] = anthropic_system_payload(&system, cache_enabled);
            }
            let resp = http
                .post(&url)
                .header("x-api-key", &resolved.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("HTTP error: {e}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(format!("API {status}: {text}"));
            }
            let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            let content = data["content"]
                .as_array()
                .and_then(|arr| arr.iter().find(|b| b["type"] == "text"))
                .and_then(|b| b["text"].as_str())
                .unwrap_or("")
                .to_string();
            Ok(content)
        }
        Provider::Ollama => {
            let url = format!("{}/api/chat", resolved.api_base);
            let api_messages = convert_messages_to_ollama(messages);
            let mut body = json!({
                "model": resolved.model_id,
                "messages": api_messages,
                "stream": false,
            });
            body["options"] = ollama_request_options(resolved);
            let resp = with_optional_bearer_auth(http.post(&url), &resolved.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| format!("HTTP error: {e}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(format!("API {status}: {text}"));
            }
            let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            Ok(data["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string())
        }
    }
}

/// Map think_level to OpenAI reasoning_effort string.
fn think_level_to_reasoning_effort(level: &str) -> &str {
    match level {
        "minimal" | "low" => "low",
        "medium" => "medium",
        "high" | "xhigh" => "high",
        _ => "medium",
    }
}

/// Map think_level to Anthropic thinking budget_tokens.
fn think_level_to_budget(level: &str) -> u64 {
    match level {
        "minimal" => 1024,
        "low" => 4096,
        "medium" => 10240,
        "high" => 16384,
        "xhigh" => 32768,
        _ => 10240,
    }
}

fn think_level_to_ollama_level(level: &str) -> &'static str {
    match level {
        "minimal" | "low" => "low",
        "medium" => "medium",
        "high" | "xhigh" => "high",
        _ => "medium",
    }
}

fn ollama_uses_think_levels(resolved: &ResolvedModel) -> bool {
    resolved
        .thinking_format
        .as_deref()
        .is_some_and(|fmt| matches!(fmt, "gpt-oss" | "ollama-gpt-oss"))
        || resolved
            .model_id
            .trim()
            .to_ascii_lowercase()
            .starts_with("gpt-oss")
}

fn ollama_think_value(resolved: &ResolvedModel, think_level: &str) -> Option<serde_json::Value> {
    if think_level == "off" {
        return if ollama_uses_think_levels(resolved) {
            None
        } else {
            Some(json!(false))
        };
    }

    if ollama_uses_think_levels(resolved) {
        Some(json!(think_level_to_ollama_level(think_level)))
    } else {
        Some(json!(true))
    }
}

fn anthropic_prompt_caching_enabled(resolved: &ResolvedModel) -> bool {
    resolved.provider == Provider::Anthropic
        && (resolved.api_base.contains("api.anthropic.com") || resolved.anthropic_prompt_caching)
}

fn anthropic_system_payload(system_prompt: &str, cache_enabled: bool) -> serde_json::Value {
    if cache_enabled {
        json!([{
            "type": "text",
            "text": system_prompt,
            "cache_control": {"type": "ephemeral"},
        }])
    } else {
        json!(system_prompt)
    }
}

fn maybe_apply_anthropic_tool_cache_control(tools: &mut [serde_json::Value], cache_enabled: bool) {
    if !cache_enabled {
        return;
    }
    if let Some(last_tool) = tools.last_mut() {
        last_tool["cache_control"] = json!({"type": "ephemeral"});
    }
}

pub(crate) async fn call_llm_stream(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> Result<LlmResponse, String> {
    // Resolve "auto": enable thinking at medium level if model supports it, else off
    let effective_level = if think_level == "auto" {
        if resolved.reasoning || resolved.thinking_format.is_some() {
            "medium"
        } else {
            "off"
        }
    } else {
        think_level
    };
    match resolved.provider {
        Provider::OpenAI => {
            call_llm_stream_openai(http, resolved, messages, tx, effective_level, extra_tools).await
        }
        Provider::Anthropic => {
            call_llm_stream_anthropic(http, resolved, messages, tx, effective_level, extra_tools)
                .await
        }
        Provider::Ollama => {
            call_llm_stream_ollama(http, resolved, messages, tx, effective_level, extra_tools).await
        }
    }
}

async fn process_openai_data_line(data: &str, tx: &LiveTx, state: &mut OpenAiStreamState) -> bool {
    if data == "[DONE]" {
        return true;
    }

    if let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) {
        if let Some(usage) = &chunk.usage {
            if let Some(value) = usage.prompt_tokens {
                state.input_tokens = Some(value);
            }
            if let Some(value) = usage.completion_tokens {
                state.output_tokens = Some(value);
            }
        }
        if let Some(choices) = chunk.choices {
            for choice in choices {
                if let Some(think_text) = &choice.delta.reasoning_content
                    && !think_text.is_empty()
                    && !state.client_gone
                {
                    if !state.reasoning_started {
                        state.reasoning_started = true;
                        state.client_gone = !live_send(tx, json!({"type":"thinking_start"})).await;
                    }
                    if !state.client_gone {
                        state.client_gone =
                            !live_send(tx, json!({"type":"thinking_delta","content":think_text}))
                                .await;
                    }
                }
                if let Some(text) = &choice.delta.content {
                    if state.reasoning_started && !state.client_gone {
                        state.reasoning_started = false;
                        state.client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
                    }
                    state.content_buf.push_str(text);
                    if !state.client_gone
                        && !live_send(tx, json!({"type":"delta","content":text})).await
                    {
                        state.client_gone = true;
                    }
                }
                if let Some(tc_deltas) = &choice.delta.tool_calls {
                    for d in tc_deltas {
                        let idx = d.index.unwrap_or(0);
                        while state.tool_calls.len() <= idx {
                            state.tool_calls.push(ToolCall {
                                id: String::new(),
                                call_type: "function".into(),
                                function: FunctionCall {
                                    name: String::new(),
                                    arguments: String::new(),
                                },
                            });
                        }
                        if let Some(id) = &d.id {
                            state.tool_calls[idx].id.clone_from(id);
                        }
                        if let Some(f) = &d.function {
                            if let Some(n) = &f.name {
                                state.tool_calls[idx].function.name.push_str(n);
                            }
                            if let Some(a) = &f.arguments {
                                state.tool_calls[idx].function.arguments.push_str(a);
                            }
                        }
                    }
                }
                if choice.finish_reason.is_some() {
                    break;
                }
            }
        }
    }

    false
}

async fn process_anthropic_sse_line(line: &str, tx: &LiveTx, state: &mut AnthropicStreamState) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    if let Some(event) = line.strip_prefix("event: ") {
        state.current_event_type = event.trim().to_string();
        return;
    }
    if let Some(data) = line.strip_prefix("data: ") {
        let data = data.trim();
        match state.current_event_type.as_str() {
            "message_start" => {
                if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data)
                    && let Some(message) = evt.message.as_ref()
                    && let Some(usage) = message.usage.as_ref()
                {
                    state.input_tokens = Some(total_anthropic_input_tokens(usage));
                    state.output_tokens = usage.output_tokens;
                }
            }
            "message_delta" => {
                if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data)
                    && let Some(usage) = evt.usage.as_ref()
                {
                    state.input_tokens = Some(total_anthropic_input_tokens(usage));
                    if let Some(value) = usage.output_tokens {
                        state.output_tokens = Some(value);
                    }
                }
            }
            "content_block_start" => {
                if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data)
                    && let Some(block) = &evt.content_block
                {
                    match block.block_type.as_str() {
                        "thinking" => {
                            state.thinking_block_idx = evt.index;
                            if !state.client_gone {
                                state.reasoning_started = true;
                                state.client_gone =
                                    !live_send(tx, json!({"type":"thinking_start"})).await;
                            }
                        }
                        "tool_use" => {
                            let idx = state.tool_calls.len();
                            state.tool_calls.push(ToolCall {
                                id: block.id.clone().unwrap_or_default(),
                                call_type: "function".into(),
                                function: FunctionCall {
                                    name: block.name.clone().unwrap_or_default(),
                                    arguments: String::new(),
                                },
                            });
                            if let Some(block_idx) = evt.index {
                                state.block_tool_idx.insert(block_idx, idx);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_delta" => {
                if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data)
                    && let Some(delta) = &evt.delta
                {
                    match delta.delta_type.as_deref() {
                        Some("thinking_delta") => {
                            if let Some(text) = &delta.thinking
                                && !text.is_empty()
                                && !state.client_gone
                            {
                                state.client_gone =
                                    !live_send(tx, json!({"type":"thinking_delta","content":text}))
                                        .await;
                            }
                        }
                        Some("text_delta") => {
                            if let Some(text) = &delta.text {
                                state.content_buf.push_str(text);
                                if !state.client_gone
                                    && !live_send(tx, json!({"type":"delta","content":text})).await
                                {
                                    state.client_gone = true;
                                }
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(json_str) = &delta.partial_json
                                && let Some(block_idx) = evt.index
                                && let Some(&tc_idx) = state.block_tool_idx.get(&block_idx)
                                && tc_idx < state.tool_calls.len()
                            {
                                state.tool_calls[tc_idx]
                                    .function
                                    .arguments
                                    .push_str(json_str);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data)
                    && state.thinking_block_idx.is_some()
                    && evt.index == state.thinking_block_idx
                {
                    state.thinking_block_idx = None;
                    state.reasoning_started = false;
                    if !state.client_gone {
                        state.client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
                    }
                }
            }
            "message_stop" => {}
            _ => {}
        }
        state.current_event_type.clear();
    }
}

async fn process_ollama_json_line(data: &str, tx: &LiveTx, state: &mut OpenAiStreamState) -> bool {
    let Ok(chunk) = serde_json::from_str::<OllamaStreamChunk>(data) else {
        return false;
    };

    if let Some(value) = chunk.prompt_eval_count {
        state.input_tokens = Some(value);
    }
    if let Some(value) = chunk.eval_count {
        state.output_tokens = Some(value);
    }

    let message = chunk.message.as_ref();
    if let Some(thinking) = message.and_then(|msg| msg.thinking.as_deref())
        && !thinking.is_empty()
        && !state.client_gone
    {
        if !state.reasoning_started {
            state.reasoning_started = true;
            state.client_gone = !live_send(tx, json!({"type":"thinking_start"})).await;
        }
        if !state.client_gone {
            state.client_gone =
                !live_send(tx, json!({"type":"thinking_delta","content":thinking})).await;
        }
    }

    let has_tool_calls = message
        .and_then(|msg| msg.tool_calls.as_ref())
        .is_some_and(|calls| !calls.is_empty());
    let has_content = message
        .and_then(|msg| msg.content.as_deref())
        .is_some_and(|content| !content.is_empty());

    if (has_content || has_tool_calls) && state.reasoning_started && !state.client_gone {
        state.reasoning_started = false;
        state.client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
    }

    if let Some(text) = message.and_then(|msg| msg.content.as_deref())
        && !text.is_empty()
    {
        state.content_buf.push_str(text);
        if !state.client_gone && !live_send(tx, json!({"type":"delta","content":text})).await {
            state.client_gone = true;
        }
    }

    if let Some(tool_calls) = message.and_then(|msg| msg.tool_calls.as_ref()) {
        for (idx, tool_call) in tool_calls.iter().enumerate() {
            while state.tool_calls.len() <= idx {
                state.tool_calls.push(ToolCall {
                    id: format!("ollama_call_{}", idx + 1),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: String::new(),
                        arguments: "{}".into(),
                    },
                });
            }
            if let Some(id) = &tool_call.id {
                state.tool_calls[idx].id.clone_from(id);
            }
            if let Some(function) = &tool_call.function {
                if let Some(name) = &function.name {
                    state.tool_calls[idx].function.name.clone_from(name);
                }
                if let Some(arguments) = &function.arguments {
                    state.tool_calls[idx].function.arguments =
                        serde_json::to_string(arguments).unwrap_or_else(|_| "{}".to_string());
                }
            }
        }
    }

    chunk.done.unwrap_or(false)
}

fn drain_sse_lines(partial_buf: &mut String, chunk: &str) -> Vec<String> {
    partial_buf.push_str(chunk);
    let mut lines: Vec<String> = partial_buf
        .split('\n')
        .map(|line| line.to_string())
        .collect();
    let leftover = lines.pop().unwrap_or_default();
    *partial_buf = leftover;
    lines
}

fn build_openai_stream_body(
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> serde_json::Value {
    let thinking_on = think_level != "off";
    let api_messages = convert_messages_to_openai(messages);
    let mut all_tools: Vec<serde_json::Value> =
        serde_json::from_value(tools::tool_definitions()).unwrap_or_default();
    all_tools.extend_from_slice(extra_tools);
    let mut body = json!({
        "model": resolved.model_id,
        "messages": api_messages,
        "tools": all_tools,
        "stream": true,
    });
    if resolved.provider == Provider::OpenAI
        && (resolved.api_base.contains("api.openai.com") || resolved.stream_include_usage)
    {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if thinking_on {
        match resolved.thinking_format.as_deref().unwrap_or("openai") {
            "qwen" => {
                body["enable_thinking"] = json!(true);
            }
            _ => {
                body["reasoning_effort"] = json!(think_level_to_reasoning_effort(think_level));
            }
        }
    }
    if let Some(max_tokens) = resolved.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    body
}

fn build_ollama_stream_body(
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> serde_json::Value {
    let api_messages = convert_messages_to_ollama(messages);
    let mut all_tools: Vec<serde_json::Value> =
        serde_json::from_value(tools::tool_definitions_ollama()).unwrap_or_default();
    all_tools.extend_from_slice(extra_tools);
    let mut body = json!({
        "model": resolved.model_id,
        "messages": api_messages,
        "tools": all_tools,
        "stream": true,
    });
    if let Some(think) = ollama_think_value(resolved, think_level) {
        body["think"] = think;
    }
    body["options"] = ollama_request_options(resolved);
    body
}

async fn consume_openai_stream<S, B>(
    stream: &mut S,
    tx: &LiveTx,
    state: &mut OpenAiStreamState,
) -> Result<(), String>
where
    S: Stream<Item = Result<B, reqwest::Error>> + Unpin,
    B: AsRef<[u8]>,
{
    let mut partial_buf = String::new();
    let mut stream_done = false;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("stream error: {error}"))?;
        let lines = drain_sse_lines(&mut partial_buf, &String::from_utf8_lossy(chunk.as_ref()));

        for line in lines {
            let line = line.trim();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(data) = line.strip_prefix("data: ")
                && process_openai_data_line(data.trim(), tx, state).await
            {
                stream_done = true;
                break;
            }
        }

        if stream_done {
            break;
        }
    }

    if !stream_done {
        let trailing = partial_buf.trim();
        if let Some(data) = trailing.strip_prefix("data: ") {
            let _ = process_openai_data_line(data.trim(), tx, state).await;
        }
    }

    Ok(())
}

fn build_anthropic_stream_body(
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> serde_json::Value {
    let thinking_on = think_level != "off";
    let (system_prompt, anthropic_msgs) = convert_messages_to_anthropic(messages);
    let base_max = resolved.max_tokens.unwrap_or(8192);
    let effective_max = if thinking_on {
        base_max.saturating_add(think_level_to_budget(think_level))
    } else {
        base_max
    };
    let mut all_tools: Vec<serde_json::Value> =
        serde_json::from_value(tools::tool_definitions_anthropic()).unwrap_or_default();
    all_tools.extend_from_slice(extra_tools);
    let cache_enabled = anthropic_prompt_caching_enabled(resolved);
    maybe_apply_anthropic_tool_cache_control(&mut all_tools, cache_enabled);
    let mut body = json!({
        "model": resolved.model_id,
        "messages": anthropic_msgs,
        "tools": all_tools,
        "max_tokens": effective_max,
        "stream": true,
    });
    if thinking_on {
        body["thinking"] = json!({
            "type": "enabled",
            "budget_tokens": think_level_to_budget(think_level),
        });
    }
    if !system_prompt.is_empty() {
        body["system"] = anthropic_system_payload(&system_prompt, cache_enabled);
    }
    body
}

async fn consume_anthropic_stream<S, B>(
    stream: &mut S,
    tx: &LiveTx,
    state: &mut AnthropicStreamState,
) -> Result<(), String>
where
    S: Stream<Item = Result<B, reqwest::Error>> + Unpin,
    B: AsRef<[u8]>,
{
    let mut partial_buf = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("stream error: {error}"))?;
        let lines = drain_sse_lines(&mut partial_buf, &String::from_utf8_lossy(chunk.as_ref()));

        for line in lines {
            process_anthropic_sse_line(&line, tx, state).await;
        }
    }

    if !partial_buf.trim().is_empty() {
        process_anthropic_sse_line(partial_buf.trim(), tx, state).await;
    }

    Ok(())
}

async fn consume_ollama_stream<S, B>(
    stream: &mut S,
    tx: &LiveTx,
    state: &mut OpenAiStreamState,
) -> Result<(), String>
where
    S: Stream<Item = Result<B, reqwest::Error>> + Unpin,
    B: AsRef<[u8]>,
{
    let mut partial_buf = String::new();
    let mut stream_done = false;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("stream error: {error}"))?;
        let lines = drain_sse_lines(&mut partial_buf, &String::from_utf8_lossy(chunk.as_ref()));

        for line in lines {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if process_ollama_json_line(line, tx, state).await {
                stream_done = true;
                break;
            }
        }

        if stream_done {
            break;
        }
    }

    if !stream_done && !partial_buf.trim().is_empty() {
        let _ = process_ollama_json_line(partial_buf.trim(), tx, state).await;
    }

    Ok(())
}

/// Maximum number of retries for transient LLM API errors (429, 5xx, connect/timeout).
const MAX_LLM_RETRIES: usize = 2;

/// Send an HTTP request with automatic retry for transient failures.
/// Retries on 429 (rate limit), 5xx (server error), and connection/timeout errors.
/// Uses exponential backoff: 1s, 2s.
async fn send_with_retry(
    _http: &Client,
    mut build: impl FnMut() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, String> {
    for attempt in 0..=MAX_LLM_RETRIES {
        if attempt > 0 {
            let delay_ms = 1000 * (1u64 << (attempt - 1));
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
        let response = build().send().await;
        match response {
            Ok(resp) => {
                let status = resp.status();
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    if attempt < MAX_LLM_RETRIES {
                        eprintln!(
                            "LLM API {status}, retrying ({}/{})",
                            attempt + 1,
                            MAX_LLM_RETRIES
                        );
                        continue;
                    }
                    let text = resp.text().await.unwrap_or_default();
                    return Err(format!(
                        "API {status} (after {} attempts): {text}",
                        attempt + 1
                    ));
                }
                if !status.is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    return Err(format!("API {status}: {text}"));
                }
                return Ok(resp);
            }
            Err(e) if attempt < MAX_LLM_RETRIES && (e.is_connect() || e.is_timeout()) => {
                eprintln!(
                    "LLM request error: {e}, retrying ({}/{})",
                    attempt + 1,
                    MAX_LLM_RETRIES
                );
                continue;
            }
            Err(e) => return Err(format!("HTTP error: {e}")),
        }
    }
    Err("LLM request failed after all retries".into())
}

async fn call_llm_stream_openai(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> Result<LlmResponse, String> {
    let url = format!("{}/chat/completions", resolved.api_base);
    let body = build_openai_stream_body(resolved, messages, think_level, extra_tools);

    let resp = send_with_retry(http, || {
        http.post(&url).bearer_auth(&resolved.api_key).json(&body)
    })
    .await?;

    let mut stream = resp.bytes_stream();
    let mut stream_state = OpenAiStreamState {
        content_buf: String::new(),
        tool_calls: Vec::new(),
        input_tokens: None,
        output_tokens: None,
        client_gone: false,
        reasoning_started: false,
    };
    consume_openai_stream(&mut stream, tx, &mut stream_state).await?;

    if stream_state.reasoning_started && !stream_state.client_gone {
        stream_state.client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
    }
    if stream_state.client_gone {
        return Err("Client disconnected".into());
    }

    build_llm_response(
        stream_state.content_buf,
        stream_state.tool_calls,
        stream_state.input_tokens,
        stream_state.output_tokens,
    )
}

async fn call_llm_stream_anthropic(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> Result<LlmResponse, String> {
    let url = format!("{}/v1/messages", resolved.api_base);
    let body = build_anthropic_stream_body(resolved, messages, think_level, extra_tools);

    let resp = send_with_retry(http, || {
        http.post(&url)
            .header("x-api-key", &resolved.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
    })
    .await?;

    let mut stream = resp.bytes_stream();
    let mut stream_state = AnthropicStreamState {
        current_event_type: String::new(),
        content_buf: String::new(),
        tool_calls: Vec::new(),
        input_tokens: None,
        output_tokens: None,
        block_tool_idx: HashMap::new(),
        client_gone: false,
        reasoning_started: false,
        thinking_block_idx: None,
    };
    consume_anthropic_stream(&mut stream, tx, &mut stream_state).await?;

    if stream_state.reasoning_started && !stream_state.client_gone {
        stream_state.client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
    }
    if stream_state.client_gone {
        return Err("Client disconnected".into());
    }

    build_llm_response(
        stream_state.content_buf,
        stream_state.tool_calls,
        stream_state.input_tokens,
        stream_state.output_tokens,
    )
}

async fn call_llm_stream_ollama(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> Result<LlmResponse, String> {
    let url = format!("{}/api/chat", resolved.api_base);
    let body = build_ollama_stream_body(resolved, messages, think_level, extra_tools);

    let resp = send_with_retry(http, || {
        with_optional_bearer_auth(http.post(&url), &resolved.api_key).json(&body)
    })
    .await?;

    let mut stream = resp.bytes_stream();
    let mut stream_state = OpenAiStreamState {
        content_buf: String::new(),
        tool_calls: Vec::new(),
        input_tokens: None,
        output_tokens: None,
        client_gone: false,
        reasoning_started: false,
    };
    consume_ollama_stream(&mut stream, tx, &mut stream_state).await?;

    if stream_state.reasoning_started && !stream_state.client_gone {
        stream_state.client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
    }
    if stream_state.client_gone {
        return Err("Client disconnected".into());
    }

    build_llm_response(
        stream_state.content_buf,
        stream_state.tool_calls,
        stream_state.input_tokens,
        stream_state.output_tokens,
    )
}

fn build_llm_response(
    content_buf: String,
    tool_calls: Vec<ToolCall>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Result<LlmResponse, String> {
    let tc = if tool_calls.is_empty() {
        None
    } else {
        Some(tool_calls)
    };
    let content = if content_buf.is_empty() {
        None
    } else {
        Some(content_buf)
    };

    Ok(LlmResponse {
        message: ChatMessage {
            role: "assistant".into(),
            content,
            tool_calls: tc,
            tool_call_id: None,
            timestamp: Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            ),
        },
        input_tokens,
        output_tokens,
    })
}

fn total_anthropic_input_tokens(usage: &AnthropicUsage) -> u64 {
    usage.input_tokens.unwrap_or(0)
        + usage.cache_creation_input_tokens.unwrap_or(0)
        + usage.cache_read_input_tokens.unwrap_or(0)
}

#[cfg(test)]
#[path = "tests/providers_tests.rs"]
mod tests;
