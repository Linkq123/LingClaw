use std::collections::HashMap;

use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;

use crate::{tools, ChatMessage, FunctionCall, Provider, ToolCall, WsTx, ws_send};

// ══════════════════════════════════════════════════════════════════════════════
//  Provider Types
// ══════════════════════════════════════════════════════════════════════════════

pub(crate) struct ResolvedModel {
    pub(crate) provider: Provider,
    pub(crate) api_base: String,
    pub(crate) api_key: String,
    pub(crate) model_id: String,
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
            let body = json!({
                "model": resolved.model_id,
                "messages": messages,
            });
            let resp = http.post(&url)
                .bearer_auth(&resolved.api_key)
                .json(&body)
                .send().await
                .map_err(|e| format!("HTTP error: {e}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(format!("API {status}: {text}"));
            }
            let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            Ok(data["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string())
        }
        Provider::Anthropic => {
            let url = format!("{}/messages", resolved.api_base);
            let (system, msgs) = convert_messages_to_anthropic(messages);
            let mut body = json!({
                "model": resolved.model_id,
                "messages": msgs,
                "max_tokens": 4096,
            });
            if !system.is_empty() {
                body["system"] = json!(system);
            }
            let resp = http.post(&url)
                .header("x-api-key", &resolved.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send().await
                .map_err(|e| format!("HTTP error: {e}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                return Err(format!("API {status}: {text}"));
            }
            let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            let content = data["content"].as_array()
                .and_then(|arr| arr.iter().find(|b| b["type"] == "text"))
                .and_then(|b| b["text"].as_str())
                .unwrap_or("")
                .to_string();
            Ok(content)
        }
    }
}

pub(crate) async fn call_llm_stream(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    tx: &mut WsTx,
) -> Result<LlmResponse, String> {
    match resolved.provider {
        Provider::OpenAI => call_llm_stream_openai(http, &resolved.api_base, &resolved.api_key, &resolved.model_id, messages, tx).await,
        Provider::Anthropic => call_llm_stream_anthropic(http, &resolved.api_base, &resolved.api_key, &resolved.model_id, messages, tx).await,
    }
}

async fn call_llm_stream_openai(
    http: &Client,
    api_base: &str,
    api_key: &str,
    model: &str,
    messages: &[ChatMessage],
    tx: &mut WsTx,
) -> Result<LlmResponse, String> {
    let url = format!("{api_base}/chat/completions");
    let body = json!({
        "model": model,
        "messages": messages,
        "tools": tools::tool_definitions(),
        "stream": true,
    });

    let resp = http
        .post(&url)
        .bearer_auth(api_key)
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
                    if let Some(choices) = chunk.choices {
                        for choice in choices {
                            // Content delta
                            if let Some(text) = &choice.delta.content {
                                content_buf.push_str(text);
                                ws_send(tx, &json!({"type":"delta","content":text})).await;
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

    build_llm_response(content_buf, tool_calls)
}

async fn call_llm_stream_anthropic(
    http: &Client,
    api_base: &str,
    api_key: &str,
    model: &str,
    messages: &[ChatMessage],
    tx: &mut WsTx,
) -> Result<LlmResponse, String> {
    let (system_prompt, anthropic_msgs) = convert_messages_to_anthropic(messages);
    let url = format!("{api_base}/v1/messages");
    let mut body = json!({
        "model": model,
        "messages": anthropic_msgs,
        "tools": tools::tool_definitions_anthropic(),
        "max_tokens": 8192,
        "stream": true,
    });
    if !system_prompt.is_empty() {
        body["system"] = json!(system_prompt);
    }

    let resp = http
        .post(&url)
        .header("x-api-key", api_key)
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
                    "content_block_start" => {
                        if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data) {
                            if let Some(block) = &evt.content_block {
                                if block.block_type == "tool_use" {
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
                            }
                        }
                    }
                    "content_block_delta" => {
                        if let Ok(evt) = serde_json::from_str::<AnthropicEvent>(data) {
                            if let Some(delta) = &evt.delta {
                                match delta.delta_type.as_deref() {
                                    Some("text_delta") => {
                                        if let Some(text) = &delta.text {
                                            content_buf.push_str(text);
                                            ws_send(tx, &json!({"type":"delta","content":text})).await;
                                        }
                                    }
                                    Some("input_json_delta") => {
                                        if let Some(json_str) = &delta.partial_json {
                                            if let Some(block_idx) = evt.index {
                                                if let Some(&tc_idx) = block_tool_idx.get(&block_idx) {
                                                    if tc_idx < tool_calls.len() {
                                                        tool_calls[tc_idx].function.arguments.push_str(json_str);
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

    build_llm_response(content_buf, tool_calls)
}

fn build_llm_response(content_buf: String, tool_calls: Vec<ToolCall>) -> Result<LlmResponse, String> {
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
        },
    })
}
