use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

use crate::{tools, ws_send, ChatMessage, FunctionCall, Provider, ToolCall, WsTx};

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
}

pub(crate) struct LlmResponse {
    pub(crate) message: ChatMessage,
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
}

// ══════════════════════════════════════════════════════════════════════════════
//  Anthropic SSE Stream Models
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Deserialize, Debug)]
struct AnthropicEvent {
    index: Option<usize>,
    delta: Option<AnthropicDelta>,
    content_block: Option<AnthropicContentBlock>,
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
            let mut body = json!({
                "model": resolved.model_id,
                "messages": msgs,
                "max_tokens": max_tokens,
            });
            if !system.is_empty() {
                body["system"] = json!(system);
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

pub(crate) async fn call_llm_stream(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    tx: &WsTx,
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
    tx: &WsTx,
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
    let mut partial_buf = String::new();
    let mut in_thinking = false;
    let mut client_gone = false;

    while let Some(chunk) = stream.next().await {
        if client_gone {
            break;
        }
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
                    if let Some(choices) = chunk.choices {
                        for choice in choices {
                            // Thinking/reasoning delta (OpenAI/Qwen)
                            if let Some(think_text) = &choice.delta.reasoning_content {
                                if !think_text.is_empty() {
                                    if !in_thinking {
                                        in_thinking = true;
                                        if !ws_send(tx, &json!({"type":"thinking_start"})).await {
                                            client_gone = true;
                                            break;
                                        }
                                    }
                                    if !ws_send(
                                        tx,
                                        &json!({"type":"thinking_delta","content":think_text}),
                                    )
                                    .await
                                    {
                                        client_gone = true;
                                        break;
                                    }
                                }
                            }
                            if client_gone {
                                break;
                            }
                            // Content delta
                            if let Some(text) = &choice.delta.content {
                                if in_thinking {
                                    in_thinking = false;
                                    let _ = ws_send(tx, &json!({"type":"thinking_done"})).await;
                                }
                                content_buf.push_str(text);
                                if !ws_send(tx, &json!({"type":"delta","content":text})).await {
                                    client_gone = true;
                                    break;
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

    if client_gone {
        return Err("Client disconnected".into());
    }

    // Ensure thinking_done is sent if the stream ended while still in thinking
    // (e.g. reasoning → tool_calls with no content delta)
    if in_thinking {
        ws_send(tx, &json!({"type":"thinking_done"})).await;
    }

    build_llm_response(content_buf, tool_calls)
}

async fn call_llm_stream_anthropic(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    tx: &WsTx,
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
        body["system"] = json!(system_prompt);
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
    let mut partial_buf = String::new();
    // Track current content block index → tool_calls index mapping
    let mut block_tool_idx: HashMap<usize, usize> = HashMap::new();
    let mut thinking_block_indices: std::collections::HashSet<usize> =
        std::collections::HashSet::new();
    let mut in_thinking = false;
    let mut client_gone = false;

    while let Some(chunk) = stream.next().await {
        if client_gone {
            break;
        }
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
                    "content_block_start" => {
                        if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data) {
                            if let Some(block) = &evt.content_block {
                                match block.block_type.as_str() {
                                    "thinking" => {
                                        if let Some(block_idx) = evt.index {
                                            thinking_block_indices.insert(block_idx);
                                        }
                                        if !in_thinking {
                                            in_thinking = true;
                                            if !ws_send(tx, &json!({"type":"thinking_start"})).await
                                            {
                                                client_gone = true;
                                            }
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
                                    _ => {
                                        // text or other block: end thinking if active
                                        if in_thinking {
                                            in_thinking = false;
                                            ws_send(tx, &json!({"type":"thinking_done"})).await;
                                        }
                                    }
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
                                            if !ws_send(
                                                tx,
                                                &json!({"type":"thinking_delta","content":text}),
                                            )
                                            .await
                                            {
                                                client_gone = true;
                                            }
                                        }
                                    }
                                    Some("text_delta") => {
                                        if in_thinking {
                                            in_thinking = false;
                                            let _ =
                                                ws_send(tx, &json!({"type":"thinking_done"})).await;
                                        }
                                        if let Some(text) = &delta.text {
                                            content_buf.push_str(text);
                                            if !ws_send(tx, &json!({"type":"delta","content":text}))
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
                        // If a thinking block just stopped, send thinking_done
                        if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data) {
                            if let Some(block_idx) = evt.index {
                                if thinking_block_indices.remove(&block_idx) && in_thinking {
                                    in_thinking = false;
                                    let _ = ws_send(tx, &json!({"type":"thinking_done"})).await;
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

    if client_gone {
        return Err("Client disconnected".into());
    }

    if in_thinking {
        ws_send(tx, &json!({"type":"thinking_done"})).await;
    }

    build_llm_response(content_buf, tool_calls)
}

fn build_llm_response(
    content_buf: String,
    tool_calls: Vec<ToolCall>,
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn think_level_to_reasoning_effort_all_levels() {
        assert_eq!(think_level_to_reasoning_effort("minimal"), "low");
        assert_eq!(think_level_to_reasoning_effort("low"), "low");
        assert_eq!(think_level_to_reasoning_effort("medium"), "medium");
        assert_eq!(think_level_to_reasoning_effort("high"), "high");
        assert_eq!(think_level_to_reasoning_effort("xhigh"), "high");
        assert_eq!(think_level_to_reasoning_effort("unknown"), "medium");
        assert_eq!(think_level_to_reasoning_effort("auto"), "medium");
    }

    #[test]
    fn think_level_to_budget_all_levels() {
        assert_eq!(think_level_to_budget("minimal"), 1024);
        assert_eq!(think_level_to_budget("low"), 4096);
        assert_eq!(think_level_to_budget("medium"), 10240);
        assert_eq!(think_level_to_budget("high"), 16384);
        assert_eq!(think_level_to_budget("xhigh"), 32768);
        assert_eq!(think_level_to_budget("unknown"), 10240);
    }

    #[test]
    fn convert_messages_to_openai_all_roles() {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: Some("you are helpful".into()),
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "user".into(),
                content: Some("hello".into()),
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: Some("hi".into()),
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: Some("result".into()),
                tool_calls: None,
                tool_call_id: Some("tc1".into()),
                timestamp: None,
            },
            ChatMessage {
                role: "unknown_role".into(),
                content: Some("skip me".into()),
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
        ];
        let out = convert_messages_to_openai(&messages);
        assert_eq!(out.len(), 4); // unknown_role skipped
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[1]["role"], "user");
        assert_eq!(out[2]["role"], "assistant");
        assert_eq!(out[3]["role"], "tool");
        assert_eq!(out[3]["tool_call_id"], "tc1");
    }

    #[test]
    fn convert_messages_to_openai_assistant_with_tool_calls() {
        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: r#"{"cmd":"ls"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        }];
        let out = convert_messages_to_openai(&messages);
        assert_eq!(out.len(), 1);
        assert!(out[0]["tool_calls"].is_array());
        assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "exec");
    }

    #[test]
    fn convert_messages_to_anthropic_system_extraction() {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: Some("system prompt".into()),
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "user".into(),
                content: Some("hello".into()),
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
        ];
        let (system, out) = convert_messages_to_anthropic(&messages);
        assert_eq!(system, "system prompt");
        assert_eq!(out.len(), 1); // system not in messages
        assert_eq!(out[0]["role"], "user");
    }

    #[test]
    fn convert_messages_to_anthropic_tool_as_user_message() {
        let messages = vec![ChatMessage {
            role: "tool".into(),
            content: Some("file contents".into()),
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        }];
        let (_, out) = convert_messages_to_anthropic(&messages);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(out[0]["content"][0]["type"], "tool_result");
        assert_eq!(out[0]["content"][0]["tool_use_id"], "tc1");
    }

    #[test]
    fn convert_messages_to_anthropic_assistant_with_tool_use() {
        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: Some("let me check".into()),
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: r#"{"cmd":"ls"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        }];
        let (_, out) = convert_messages_to_anthropic(&messages);
        assert_eq!(out.len(), 1);
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2); // text block + tool_use block
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[1]["name"], "exec");
    }

    #[test]
    fn convert_messages_to_anthropic_empty_assistant_gets_placeholder() {
        let messages = vec![ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        }];
        let (_, out) = convert_messages_to_anthropic(&messages);
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "");
    }

    #[test]
    fn build_llm_response_empty_content_and_no_tools() {
        let resp = build_llm_response(String::new(), vec![]).unwrap();
        assert!(resp.message.content.is_none());
        assert!(resp.message.tool_calls.is_none());
        assert_eq!(resp.message.role, "assistant");
    }

    #[test]
    fn build_llm_response_with_content_and_tools() {
        let resp = build_llm_response(
            "thinking...".into(),
            vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: "{}".into(),
                },
            }],
        )
        .unwrap();
        assert_eq!(resp.message.content.as_deref(), Some("thinking..."));
        assert_eq!(resp.message.tool_calls.as_ref().unwrap().len(), 1);
    }
}
