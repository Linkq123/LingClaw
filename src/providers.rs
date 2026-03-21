use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

use crate::{live_send, tools, ChatMessage, FunctionCall, LiveTx, Provider, ToolCall};

// ══════════════════════════════════════════════════════════════════════════════
//  Provider Types
// ══════════════════════════════════════════════════════════════════════════════

pub(crate) struct ResolvedModel {
    pub(crate) provider: Provider,
    pub(crate) api_base: String,
    pub(crate) api_key: String,
    pub(crate) model_id: String,
    pub(crate) reasoning: bool,
    /// From model config `compat.thinkingFormat`: "qwen", "openai", "anthropic", etc.
    pub(crate) thinking_format: Option<String>,
    /// From model config `maxTokens`.
    pub(crate) max_tokens: Option<u64>,
    pub(crate) stream_include_usage: bool,
    pub(crate) anthropic_prompt_caching: bool,
}

pub(crate) struct LlmResponse {
    pub(crate) message: ChatMessage,
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
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
                if let Some(text) = &msg.content {
                    if !text.is_empty() {
                        content_blocks.push(json!({"type": "text", "text": text}));
                    }
                }
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        let input: serde_json::Value =
                            serde_json::from_str(&tc.function.arguments).unwrap_or(json!({}));
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
    }
}

async fn call_llm_stream_openai(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> Result<LlmResponse, String> {
    let thinking_on = think_level != "off";
    let url = format!("{}/chat/completions", resolved.api_base);
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
        let fmt = resolved.thinking_format.as_deref().unwrap_or("openai");
        match fmt {
            "qwen" => {
                body["enable_thinking"] = json!(true);
            }
            _ => {
                // OpenAI-compatible reasoning_effort
                body["reasoning_effort"] = json!(think_level_to_reasoning_effort(think_level));
            }
        }
    }
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

    let mut stream = resp.bytes_stream();
    let mut content_buf = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut input_tokens: Option<u64> = None;
    let mut output_tokens: Option<u64> = None;
    let mut partial_buf = String::new();
    let mut client_gone = false;
    let mut reasoning_started = false;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("stream error: {e}"))?;
        partial_buf.push_str(&String::from_utf8_lossy(&chunk));

        let lines: Vec<&str> = partial_buf.split('\n').collect();
        let (complete, rest) = lines.split_at(lines.len() - 1);
        let leftover = rest.first().copied().unwrap_or("");

        for line in complete {
            let line = line.trim();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(data) = line.strip_prefix("data: ") {
                let data = data.trim();
                if data == "[DONE]" {
                    break;
                }
                if let Ok(chunk) = serde_json::from_str::<StreamChunk>(data) {
                    if let Some(usage) = &chunk.usage {
                        if let Some(value) = usage.prompt_tokens {
                            input_tokens = Some(value);
                        }
                        if let Some(value) = usage.completion_tokens {
                            output_tokens = Some(value);
                        }
                    }
                    if let Some(choices) = chunk.choices {
                        for choice in choices {
                            // Stream reasoning deltas live so the panel
                            // appears while the model is still thinking.
                            if let Some(think_text) = &choice.delta.reasoning_content {
                                if !think_text.is_empty() && !client_gone {
                                    if !reasoning_started {
                                        reasoning_started = true;
                                        client_gone = !live_send(tx, json!({"type":"thinking_start"})).await;
                                    }
                                    if !client_gone {
                                        client_gone = !live_send(tx, json!({"type":"thinking_delta","content":think_text})).await;
                                    }
                                }
                            }
                            // Content delta — close reasoning panel first
                            if let Some(text) = &choice.delta.content {
                                if reasoning_started && !client_gone {
                                    reasoning_started = false;
                                    client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
                                }
                                content_buf.push_str(text);
                                if !client_gone
                                    && !live_send(tx, json!({"type":"delta","content":text})).await
                                {
                                    client_gone = true;
                                }
                            }
                            // Tool call deltas
                            if let Some(tc_deltas) = &choice.delta.tool_calls {
                                for d in tc_deltas {
                                    let idx = d.index.unwrap_or(0);
                                    while tool_calls.len() <= idx {
                                        tool_calls.push(ToolCall {
                                            id: String::new(),
                                            call_type: "function".into(),
                                            function: FunctionCall {
                                                name: String::new(),
                                                arguments: String::new(),
                                            },
                                        });
                                    }
                                    if let Some(id) = &d.id {
                                        tool_calls[idx].id.clone_from(id);
                                    }
                                    if let Some(f) = &d.function {
                                        if let Some(n) = &f.name {
                                            tool_calls[idx].function.name.push_str(n);
                                        }
                                        if let Some(a) = &f.arguments {
                                            tool_calls[idx].function.arguments.push_str(a);
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
            }
        }
        partial_buf = leftover.to_string();
    }

    if reasoning_started && !client_gone {
        client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
    }
    if client_gone {
        return Err("Client disconnected".into());
    }

    build_llm_response(content_buf, tool_calls, input_tokens, output_tokens)
}

async fn call_llm_stream_anthropic(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> Result<LlmResponse, String> {
    let thinking_on = think_level != "off";
    let (system_prompt, anthropic_msgs) = convert_messages_to_anthropic(messages);
    let url = format!("{}/v1/messages", resolved.api_base);
    let base_max = resolved.max_tokens.unwrap_or(8192);
    let effective_max = if thinking_on {
        let budget = think_level_to_budget(think_level);
        base_max + budget
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

    let resp = http
        .post(&url)
        .header("x-api-key", &resolved.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("API {status}: {text}"));
    }

    let mut stream = resp.bytes_stream();
    let mut content_buf = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut input_tokens: Option<u64> = None;
    let mut output_tokens: Option<u64> = None;
    let mut partial_buf = String::new();
    // Track current content block index → tool_calls index mapping
    let mut block_tool_idx: HashMap<usize, usize> = HashMap::new();
    let mut client_gone = false;
    let mut reasoning_started = false;
    let mut thinking_block_idx: Option<usize> = None;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("stream error: {e}"))?;
        partial_buf.push_str(&String::from_utf8_lossy(&chunk));

        let lines: Vec<&str> = partial_buf.split('\n').collect();
        let (complete, rest) = lines.split_at(lines.len() - 1);
        let leftover = rest.first().copied().unwrap_or("");

        let mut current_event_type = String::new();
        for line in complete {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(event) = line.strip_prefix("event: ") {
                current_event_type = event.trim().to_string();
                continue;
            }
            if let Some(data) = line.strip_prefix("data: ") {
                let data = data.trim();
                match current_event_type.as_str() {
                    "message_start" => {
                        if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data) {
                            if let Some(message) = evt.message.as_ref() {
                                if let Some(usage) = message.usage.as_ref() {
                                    input_tokens = Some(total_anthropic_input_tokens(usage));
                                    output_tokens = usage.output_tokens;
                                }
                            }
                        }
                    }
                    "message_delta" => {
                        if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data) {
                            if let Some(usage) = evt.usage.as_ref() {
                                input_tokens = Some(total_anthropic_input_tokens(usage));
                                if let Some(value) = usage.output_tokens {
                                    output_tokens = Some(value);
                                }
                            }
                        }
                    }
                    "content_block_start" => {
                        if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data) {
                            if let Some(block) = &evt.content_block {
                                match block.block_type.as_str() {
                                    "thinking" => {
                                        thinking_block_idx = evt.index;
                                        if !client_gone {
                                            reasoning_started = true;
                                            client_gone = !live_send(tx, json!({"type":"thinking_start"})).await;
                                        }
                                    }
                                    "tool_use" => {
                                        let idx = tool_calls.len();
                                        tool_calls.push(ToolCall {
                                            id: block.id.clone().unwrap_or_default(),
                                            call_type: "function".into(),
                                            function: FunctionCall {
                                                name: block.name.clone().unwrap_or_default(),
                                                arguments: String::new(),
                                            },
                                        });
                                        if let Some(block_idx) = evt.index {
                                            block_tool_idx.insert(block_idx, idx);
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    "content_block_delta" => {
                        if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data) {
                            if let Some(delta) = &evt.delta {
                                match delta.delta_type.as_deref() {
                                    Some("thinking_delta") => {
                                        if let Some(text) = &delta.thinking {
                                            if !text.is_empty() && !client_gone {
                                                client_gone = !live_send(tx, json!({"type":"thinking_delta","content":text})).await;
                                            }
                                        }
                                    }
                                    Some("text_delta") => {
                                        if let Some(text) = &delta.text {
                                            content_buf.push_str(text);
                                            if !client_gone
                                                && !live_send(
                                                    tx,
                                                    json!({"type":"delta","content":text}),
                                                )
                                                .await
                                            {
                                                client_gone = true;
                                            }
                                        }
                                    }
                                    Some("input_json_delta") => {
                                        if let Some(json_str) = &delta.partial_json {
                                            if let Some(block_idx) = evt.index {
                                                if let Some(&tc_idx) =
                                                    block_tool_idx.get(&block_idx)
                                                {
                                                    if tc_idx < tool_calls.len() {
                                                        tool_calls[tc_idx]
                                                            .function
                                                            .arguments
                                                            .push_str(json_str);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    "content_block_stop" => {
                        if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data) {
                            if thinking_block_idx.is_some() && evt.index == thinking_block_idx {
                                thinking_block_idx = None;
                                reasoning_started = false;
                                if !client_gone {
                                    client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
                                }
                            }
                        }
                    }
                    "message_stop" => {
                        // End of message
                    }
                    _ => {}
                }
                current_event_type.clear();
            }
        }
        partial_buf = leftover.to_string();
    }

    if reasoning_started && !client_gone {
        client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
    }
    if client_gone {
        return Err("Client disconnected".into());
    }

    build_llm_response(content_buf, tool_calls, input_tokens, output_tokens)
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
