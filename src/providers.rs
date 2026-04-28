use std::{
    borrow::Cow,
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::{Stream, StreamExt};
use reqwest::{Client, RequestBuilder};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use base64::Engine;

use crate::{
    AnthropicThinkingBlock, ChatMessage, FunctionCall, LiveTx, Provider, ToolCall, image_uploads,
    live_send, tools,
};

/// Maximum size for a single image fetched for Ollama base64 encoding (10 MB).
const MAX_IMAGE_FETCH_BYTES: usize = 10 * 1024 * 1024;
const IMAGE_CACHE_DIR_NAME: &str = ".image-cache";

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

pub(crate) struct SimpleLlmResponse {
    pub(crate) content: String,
    pub(crate) input_tokens: Option<u64>,
    pub(crate) output_tokens: Option<u64>,
}

struct OpenAiStreamState {
    content_buf: String,
    thinking_buf: String,
    tool_calls: Vec<ToolCall>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    client_gone: bool,
    reasoning_started: bool,
}

struct AnthropicStreamState {
    current_event_type: String,
    content_buf: String,
    thinking_buf: String,
    thinking_blocks: Vec<AnthropicThinkingBlock>,
    tool_calls: Vec<ToolCall>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    block_tool_idx: HashMap<usize, usize>,
    block_thinking_idx: HashMap<usize, usize>,
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
    signature: Option<String>,
    partial_json: Option<String>,
}

#[derive(Deserialize, Debug)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    id: Option<String>,
    name: Option<String>,
    thinking: Option<String>,
    signature: Option<String>,
    data: Option<String>,
}

// ══════════════════════════════════════════════════════════════════════════════
//  Gemini Stream Models
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerateResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    usage_metadata: Option<GeminiUsage>,
    prompt_feedback: Option<GeminiPromptFeedback>,
    error: Option<GeminiApiError>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    finish_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GeminiContent {
    parts: Option<Vec<GeminiPart>>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    text: Option<String>,
    thought: Option<bool>,
    thought_signature: Option<String>,
    function_call: Option<GeminiFunctionCall>,
}

#[derive(Deserialize, Debug)]
struct GeminiFunctionCall {
    id: Option<String>,
    name: Option<String>,
    args: Option<Value>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct GeminiUsage {
    prompt_token_count: Option<u64>,
    candidates_token_count: Option<u64>,
    thoughts_token_count: Option<u64>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct GeminiPromptFeedback {
    block_reason: Option<String>,
}

#[derive(Deserialize, Debug)]
struct GeminiApiError {
    code: Option<u16>,
    message: Option<String>,
    status: Option<String>,
}

// ══════════════════════════════════════════════════════════════════════════════
//  Message Conversion
// ══════════════════════════════════════════════════════════════════════════════

fn is_official_openai_api_base(api_base: &str) -> bool {
    reqwest::Url::parse(api_base)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host.eq_ignore_ascii_case("api.openai.com"))
        })
        .unwrap_or(false)
}

fn openai_prefers_null_tool_call_content_with_opt_in(
    api_base: &str,
    explicit_opt_in: bool,
) -> bool {
    is_official_openai_api_base(api_base) || explicit_opt_in
}

fn openai_prefers_null_tool_call_content(resolved: &ResolvedModel) -> bool {
    openai_prefers_null_tool_call_content_with_opt_in(
        &resolved.api_base,
        crate::config::parse_boolish_env("LINGCLAW_OPENAI_NULL_TOOL_CALL_CONTENT") == Some(true),
    )
}

fn openai_supports_reasoning_controls(resolved: &ResolvedModel) -> bool {
    is_official_openai_api_base(&resolved.api_base) || resolved.thinking_format.is_some()
}

/// Convert internal messages to clean OpenAI API format (strips timestamps and
/// extra fields so the provider receives only role/content/tool_calls/tool_call_id).
///
/// When `thinking_format` is `"deepseek-v4"`, assistant messages include
/// `reasoning_content` from the internal `thinking` field so that the
/// DeepSeek API receives the reasoning chain for context (required when
/// tool calls are present).
fn deepseek_thinking_replay_needs_tool_turn_repair(message: &ChatMessage) -> bool {
    message.role == "assistant"
        && message
            .tool_calls
            .as_ref()
            .is_some_and(|tool_calls| !tool_calls.is_empty())
        && message.thinking.as_deref().is_none_or(str::is_empty)
}

fn deepseek_reasoning_replay_placeholder(has_tool_calls: bool) -> String {
    if has_tool_calls {
        "Historical assistant tool turn replayed without original reasoning_content.".to_string()
    } else {
        "Historical assistant response replayed without original reasoning_content.".to_string()
    }
}

fn summarize_deepseek_tool_turn_as_plain_assistant(
    assistant: &ChatMessage,
    tool_messages: &[ChatMessage],
) -> ChatMessage {
    let replay_reasoning =
        "Historical tool turn summarized because the original DeepSeek response omitted reasoning_content."
            .to_string();
    let mut result_by_id: HashMap<&str, &str> = HashMap::new();
    for tool_message in tool_messages {
        if let Some(tool_call_id) = tool_message.tool_call_id.as_deref() {
            result_by_id.insert(tool_call_id, tool_message.content.as_deref().unwrap_or(""));
        }
    }

    let mut lines = vec![
        "Prior tool turn replayed as plain context because DeepSeek omitted reasoning_content."
            .to_string(),
    ];
    if let Some(content) = assistant
        .content
        .as_deref()
        .filter(|content| !content.is_empty())
    {
        lines.push(format!("Assistant content: {content}"));
    }
    if let Some(tool_calls) = &assistant.tool_calls {
        for tool_call in tool_calls {
            lines.push(format!(
                "Tool {} args: {}",
                tool_call.function.name, tool_call.function.arguments
            ));
            match result_by_id.get(tool_call.id.as_str()) {
                Some(result) => lines.push(format!("Tool result: {result}")),
                None => lines.push(format!("Tool result missing for id: {}", tool_call.id)),
            }
        }
    }
    for tool_message in tool_messages {
        let Some(tool_call_id) = tool_message.tool_call_id.as_deref() else {
            continue;
        };
        if assistant.tool_calls.as_ref().is_some_and(|tool_calls| {
            tool_calls
                .iter()
                .any(|tool_call| tool_call.id == tool_call_id)
        }) {
            continue;
        }
        lines.push(format!(
            "Tool result for unmatched id {}: {}",
            tool_call_id,
            tool_message.content.as_deref().unwrap_or("")
        ));
    }

    ChatMessage {
        role: "assistant".into(),
        content: Some(lines.join("\n")),
        images: None,
        thinking: Some(replay_reasoning),
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: assistant.timestamp,
    }
}

fn repair_deepseek_thinking_tool_history(messages: &[ChatMessage]) -> Vec<ChatMessage> {
    let mut repaired = Vec::with_capacity(messages.len());
    let mut idx = 0;

    while idx < messages.len() {
        let message = &messages[idx];
        if !deepseek_thinking_replay_needs_tool_turn_repair(message) {
            repaired.push(message.clone());
            idx += 1;
            continue;
        }

        let mut next = idx + 1;
        while next < messages.len() && messages[next].role == "tool" {
            next += 1;
        }
        repaired.push(summarize_deepseek_tool_turn_as_plain_assistant(
            message,
            &messages[idx + 1..next],
        ));
        idx = next;
    }

    repaired
}

fn prepare_openai_messages_for_request<'a>(
    resolved: &ResolvedModel,
    messages: &'a [ChatMessage],
    repair_deepseek_history: bool,
) -> Cow<'a, [ChatMessage]> {
    if repair_deepseek_history
        && resolved.provider == Provider::OpenAI
        && resolved.thinking_format.as_deref() == Some("deepseek-v4")
        && messages
            .iter()
            .any(deepseek_thinking_replay_needs_tool_turn_repair)
    {
        Cow::Owned(repair_deepseek_thinking_tool_history(messages))
    } else {
        Cow::Borrowed(messages)
    }
}

fn convert_messages_to_openai_with_options(
    messages: &[ChatMessage],
    null_tool_call_content: bool,
    thinking_format: Option<&str>,
) -> Vec<serde_json::Value> {
    // Many OpenAI-compatible providers reject image_url content in user
    // messages when the conversation also contains tool-role messages
    // (400 InvalidParameter).  Pre-scan for tool messages and strip images
    // from user messages when any are present.
    let has_tool_messages = messages.iter().any(|m| m.role == "tool");

    let mut out = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => {
                out.push(json!({
                    "role": "system",
                    "content": msg.content.as_deref().unwrap_or(""),
                }));
            }
            "user" => {
                if !has_tool_messages
                    && let Some(images) = &msg.images
                    && !images.is_empty()
                {
                    let mut parts: Vec<Value> = vec![json!({
                        "type": "text",
                        "text": msg.content.as_deref().unwrap_or("")
                    })];
                    for img in images {
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": {"url": &img.url}
                        }));
                    }
                    out.push(json!({"role": "user", "content": parts}));
                } else {
                    out.push(json!({
                        "role": "user",
                        "content": msg.content.as_deref().unwrap_or(""),
                    }));
                }
            }
            "assistant" => {
                let mut item = json!({
                    "role": "assistant",
                });
                let assistant_text = msg.content.as_deref().filter(|content| !content.is_empty());
                if null_tool_call_content && msg.tool_calls.is_some() && assistant_text.is_none() {
                    item["content"] = Value::Null;
                } else {
                    item["content"] = json!(assistant_text.unwrap_or(""));
                }
                if let Some(tool_calls) = &msg.tool_calls {
                    item["tool_calls"] = json!(
                        tool_calls
                            .iter()
                            .map(|tool_call| json!({
                                "id": tool_call.id,
                                "type": tool_call.call_type,
                                "function": {
                                    "name": tool_call.function.name,
                                    "arguments": tool_call.function.arguments,
                                }
                            }))
                            .collect::<Vec<_>>()
                    );
                }
                if thinking_format == Some("deepseek-v4") {
                    // DeepSeek may reject historical assistant messages in thinking
                    // mode when reasoning_content is absent, including plain
                    // assistant replies. Preserve original reasoning when
                    // available; otherwise synthesize a stable placeholder so
                    // legacy sessions continue to replay safely.
                    let reasoning = msg
                        .thinking
                        .as_deref()
                        .filter(|reasoning| !reasoning.is_empty())
                        .map(ToOwned::to_owned)
                        .unwrap_or_else(|| {
                            deepseek_reasoning_replay_placeholder(msg.tool_calls.is_some())
                        });
                    item["reasoning_content"] = json!(reasoning);
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
fn append_anthropic_thinking_blocks(
    content_blocks: &mut Vec<serde_json::Value>,
    msg: &ChatMessage,
) {
    let Some(blocks) = &msg.anthropic_thinking_blocks else {
        return;
    };

    for block in blocks {
        match block.block_type.as_str() {
            "thinking" => {
                if let (Some(thinking), Some(signature)) =
                    (block.thinking.as_deref(), block.signature.as_deref())
                    && !signature.is_empty()
                {
                    content_blocks.push(json!({
                        "type": "thinking",
                        "thinking": thinking,
                        "signature": signature,
                    }));
                }
            }
            "redacted_thinking" => {
                if let Some(data) = block.data.as_deref()
                    && !data.is_empty()
                {
                    content_blocks.push(json!({
                        "type": "redacted_thinking",
                        "data": data,
                    }));
                }
            }
            _ => {}
        }
    }
}

fn convert_messages_to_anthropic(messages: &[ChatMessage]) -> (String, Vec<serde_json::Value>) {
    let mut system = String::new();
    let mut out: Vec<serde_json::Value> = Vec::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => {
                system = msg.content.clone().unwrap_or_default();
            }
            "user" => {
                if let Some(images) = &msg.images
                    && !images.is_empty()
                {
                    let mut parts: Vec<Value> = vec![json!({
                        "type": "text",
                        "text": msg.content.as_deref().unwrap_or("")
                    })];
                    for img in images {
                        parts.push(json!({
                            "type": "image",
                            "source": {"type": "url", "url": &img.url}
                        }));
                    }
                    out.push(json!({"role": "user", "content": parts}));
                } else {
                    out.push(json!({
                        "role": "user",
                        "content": msg.content.as_deref().unwrap_or(""),
                    }));
                }
            }
            "assistant" => {
                let mut content_blocks: Vec<serde_json::Value> = Vec::new();
                // Structured Anthropic thinking blocks can be round-tripped.
                // Legacy flat `msg.thinking` text remains UI-only because it
                // has no signature and would be rejected by the API.
                append_anthropic_thinking_blocks(&mut content_blocks, msg);
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

fn materialize_image_urls(
    messages: &[ChatMessage],
    s3_cfg: Option<&crate::config::S3Config>,
) -> Result<Vec<ChatMessage>, String> {
    let mut hydrated = messages.to_vec();
    for msg in &mut hydrated {
        if let Some(images) = msg.images.as_mut() {
            for image in images {
                image.url = image_uploads::resolve_image_url(
                    &image.url,
                    image.s3_object_key.as_deref(),
                    s3_cfg,
                )?;
            }
        }
    }
    Ok(hydrated)
}

fn is_trusted_uploaded_image(
    image: &crate::ImageAttachment,
    s3_cfg: Option<&crate::config::S3Config>,
) -> bool {
    s3_cfg.is_some()
        && image
            .s3_object_key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
}

async fn fetch_image_base64_for_message(
    image: &crate::ImageAttachment,
    safe_http: &Client,
    s3_cfg: Option<&crate::config::S3Config>,
) -> Result<String, String> {
    if is_trusted_uploaded_image(image, s3_cfg) {
        fetch_single_image_base64_trusted(&image.url, safe_http).await
    } else {
        fetch_single_image_base64(&image.url, safe_http).await
    }
}

/// Pre-fetch all image URLs in the message list and return a URL→base64 map.
/// Ollama requires base64-encoded image data rather than URLs.
/// Uses cached `data` from `ImageAttachment` when available (intake pre-fetch).
/// Falls back to a network fetch for legacy messages that lack cached data.
/// Trusted local uploads identified by `s3_object_key` bypass SSRF checks after
/// `materialize_image_urls()` regenerates their presigned URL from the object key.
/// Individual image fetch failures are logged as warnings and skipped unless
/// `strict_missing` is set; only client construction failure always returns `Err`.
async fn fetch_images_base64(
    messages: &[ChatMessage],
    workspace: &Path,
    s3_cfg: Option<&crate::config::S3Config>,
    strict_missing: bool,
) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    // Build the safe HTTP client once for the entire batch (legacy fallback path).
    let mut safe_http: Option<Client> = None;
    for msg in messages {
        if msg.role != "user" {
            continue;
        }
        if let Some(images) = &msg.images {
            for img in images {
                if map.contains_key(&img.url) {
                    continue;
                }
                let b64 = if let Some(cached) = &img.data {
                    Some(cached.clone())
                } else if let Some(cache_path) = &img.cache_path {
                    let cached = match resolve_image_cache_path(cache_path, workspace) {
                        Ok(valid_path) => tokio::fs::read_to_string(&valid_path).await.ok(),
                        Err(err) => {
                            eprintln!(
                                "Warning: ignoring suspicious image cache path {}: {}",
                                cache_path, err
                            );
                            None
                        }
                    };
                    if let Some(cached) = cached {
                        Some(cached)
                    } else {
                        let http = match &safe_http {
                            Some(c) => c,
                            None => {
                                safe_http = Some(build_image_fetch_client()?);
                                safe_http.as_ref().ok_or_else(|| {
                                    "Failed to initialize image fetch client".to_string()
                                })?
                            }
                        };
                        match fetch_image_base64_for_message(img, http, s3_cfg).await {
                            Ok(cached) => Some(cached),
                            Err(err) => {
                                if strict_missing {
                                    return Err(format!(
                                        "Failed to fetch image {} for model request: {}",
                                        img.url, err
                                    ));
                                }
                                eprintln!(
                                    "Warning: skipping uncached historical image {}: {}",
                                    img.url, err
                                );
                                None
                            }
                        }
                    }
                } else {
                    // Legacy fallback: image was stored before intake pre-fetch existed.
                    let http = match &safe_http {
                        Some(c) => c,
                        None => {
                            safe_http = Some(build_image_fetch_client()?);
                            safe_http.as_ref().ok_or_else(|| {
                                "Failed to initialize image fetch client".to_string()
                            })?
                        }
                    };
                    match fetch_image_base64_for_message(img, http, s3_cfg).await {
                        Ok(cached) => Some(cached),
                        Err(err) => {
                            if strict_missing {
                                return Err(format!(
                                    "Failed to fetch image {} for model request: {}",
                                    img.url, err
                                ));
                            }
                            eprintln!(
                                "Warning: skipping uncached historical image {}: {}",
                                img.url, err
                            );
                            None
                        }
                    }
                };
                if let Some(b64) = b64 {
                    map.insert(img.url.clone(), b64);
                }
            }
        }
    }
    Ok(map)
}

/// Build a one-off HTTP client suitable for image fetching:
/// redirects disabled (SSRF defense), 15 s timeout.
pub(crate) fn build_image_fetch_client() -> Result<Client, String> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Failed to create safe HTTP client for image fetch: {e}"))
}

fn image_cache_dir(workspace: &Path) -> PathBuf {
    workspace.join(IMAGE_CACHE_DIR_NAME)
}

fn next_image_cache_path(workspace: &Path, url: &str) -> Result<PathBuf, String> {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    let hash = hasher.finish();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("Failed to compute cache timestamp: {e}"))?
        .as_nanos();
    Ok(image_cache_dir(workspace).join(format!("{nanos:032x}-{hash:016x}.b64")))
}

pub(crate) async fn persist_image_base64_cache(
    workspace: &Path,
    url: &str,
    b64: &str,
) -> Result<String, String> {
    let dir = image_cache_dir(workspace);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create image cache directory: {e}"))?;
    let path = next_image_cache_path(workspace, url)?;
    tokio::fs::write(&path, b64)
        .await
        .map_err(|e| format!("Failed to write image cache file: {e}"))?;
    Ok(path.to_string_lossy().to_string())
}

/// Validate that a deserialized `cache_path` is a plausible `.b64` file
/// inside an `.image-cache` directory.  Prevents tampered session JSON from
/// tricking the server into reading arbitrary files.
fn resolve_image_cache_path(cache_path: &str, workspace: &Path) -> Result<PathBuf, String> {
    let workspace_root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let cache_root = workspace_root.join(IMAGE_CACHE_DIR_NAME);
    let raw = Path::new(cache_path);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        workspace_root.join(raw)
    };
    if candidate.extension().and_then(|e| e.to_str()) != Some("b64") {
        return Err("cache file must use .b64 extension".into());
    }
    let canonical_candidate = candidate
        .canonicalize()
        .map_err(|e| format!("cache file is not readable: {e}"))?;
    if !canonical_candidate.starts_with(&cache_root) {
        return Err(format!(
            "cache file is outside session image cache '{}'",
            cache_root.display()
        ));
    }
    Ok(canonical_candidate)
}

async fn fetch_single_image_base64_with_policy(
    url: &str,
    safe_http: &Client,
    enforce_ssrf: bool,
) -> Result<String, String> {
    if enforce_ssrf && let Some(ssrf_msg) = tools::net::check_ssrf(url).await {
        return Err(format!("Image fetch blocked ({url}): {ssrf_msg}"));
    }

    let resp = safe_http
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch image {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "Failed to fetch image {url} (HTTP {})",
            resp.status()
        ));
    }

    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !image_uploads::is_supported_image_content_type(content_type) {
        return Err(format!(
            "Image fetch returned unsupported content type '{}' for {}",
            if content_type.is_empty() {
                "unknown"
            } else {
                content_type
            },
            url
        ));
    }

    // Pre-check Content-Length header when available to reject obviously
    // oversized bodies without reading any data.
    if let Some(cl) = resp.content_length()
        && cl as usize > MAX_IMAGE_FETCH_BYTES
    {
        return Err(format!(
            "Image too large ({cl} bytes, max {MAX_IMAGE_FETCH_BYTES}): {url}"
        ));
    }

    // Streaming accumulation with a running size cap.
    let mut buf = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("Failed to read image body {url}: {e}"))?;
        if buf.len() + chunk.len() > MAX_IMAGE_FETCH_BYTES {
            return Err(format!(
                "Image too large (exceeded {MAX_IMAGE_FETCH_BYTES} bytes): {url}"
            ));
        }
        buf.extend_from_slice(&chunk);
    }

    Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
}

/// Fetch a single image URL and return its base64-encoded content.
///
/// Performs SSRF check, checks Content-Length before downloading, and
/// enforces a streaming size cap to prevent memory exhaustion from
/// attacker-controlled URLs.  The caller supplies a no-redirect `Client`
/// (via [`build_image_fetch_client`]) so that batches of images reuse one
/// connection pool.
pub(crate) async fn fetch_single_image_base64(
    url: &str,
    safe_http: &Client,
) -> Result<String, String> {
    fetch_single_image_base64_with_policy(url, safe_http, true).await
}

pub(crate) async fn fetch_single_image_base64_trusted(
    url: &str,
    safe_http: &Client,
) -> Result<String, String> {
    fetch_single_image_base64_with_policy(url, safe_http, false).await
}

fn convert_messages_to_ollama(
    messages: &[ChatMessage],
    images_b64: &HashMap<String, String>,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut tool_names_by_id: HashMap<String, String> = HashMap::new();

    for msg in messages {
        match msg.role.as_str() {
            "system" => {
                out.push(json!({
                    "role": "system",
                    "content": msg.content.as_deref().unwrap_or(""),
                }));
            }
            "user" => {
                let mut item = json!({
                    "role": "user",
                    "content": msg.content.as_deref().unwrap_or(""),
                });
                if let Some(images) = &msg.images
                    && !images.is_empty()
                {
                    let b64_list: Vec<&str> = images
                        .iter()
                        .filter_map(|i| images_b64.get(&i.url).map(|s| s.as_str()))
                        .collect();
                    if !b64_list.is_empty() {
                        item["images"] = json!(b64_list);
                    }
                }
                out.push(item);
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

fn gemini_image_mime_type(b64: &str) -> Option<&'static str> {
    base64::engine::general_purpose::STANDARD
        .decode(b64)
        .ok()
        .and_then(|bytes| image_uploads::detect_image_upload_content_type(&bytes))
}

fn parse_function_args_object(args: &str) -> serde_json::Map<String, Value> {
    serde_json::from_str::<Value>(args)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn convert_messages_to_gemini(
    messages: &[ChatMessage],
    images_b64: &HashMap<String, String>,
) -> Result<(Option<serde_json::Value>, Vec<serde_json::Value>), String> {
    let mut system_parts: Vec<Value> = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    let mut tool_names_by_id: HashMap<String, String> = HashMap::new();

    let mut index = 0;
    while index < messages.len() {
        let msg = &messages[index];
        match msg.role.as_str() {
            "system" => {
                if let Some(content) = msg.content.as_deref()
                    && !content.is_empty()
                {
                    system_parts.push(json!({ "text": content }));
                }
            }
            "user" => {
                let mut parts = Vec::new();
                let text = msg.content.as_deref().unwrap_or("");
                if !text.is_empty() {
                    parts.push(json!({ "text": text }));
                }
                if let Some(images) = &msg.images {
                    for image in images {
                        if let Some(b64) = images_b64.get(&image.url) {
                            let Some(mime_type) = gemini_image_mime_type(b64) else {
                                return Err(format!(
                                    "Gemini image data for {} is not a supported PNG/JPEG payload",
                                    image.url
                                ));
                            };
                            parts.push(json!({
                                "inlineData": {
                                    "mimeType": mime_type,
                                    "data": b64,
                                }
                            }));
                        }
                    }
                }
                if parts.is_empty() {
                    parts.push(json!({ "text": "" }));
                }
                contents.push(json!({ "role": "user", "parts": parts }));
            }
            "assistant" => {
                let mut parts = Vec::new();
                if let Some(content) = msg.content.as_deref()
                    && !content.is_empty()
                {
                    parts.push(json!({ "text": content }));
                }
                if let Some(tool_calls) = &msg.tool_calls {
                    for tool_call in tool_calls {
                        if !tool_call.id.is_empty() {
                            tool_names_by_id
                                .insert(tool_call.id.clone(), tool_call.function.name.clone());
                        }
                        let mut function_call = json!({
                            "name": tool_call.function.name,
                            "args": parse_function_args_object(&tool_call.function.arguments),
                        });
                        if !tool_call.id.is_empty() {
                            function_call["id"] = json!(tool_call.id);
                        }
                        let mut part = json!({ "functionCall": function_call });
                        if let Some(signature) = tool_call
                            .gemini_thought_signature
                            .as_deref()
                            .filter(|signature| !signature.is_empty())
                        {
                            part["thoughtSignature"] = json!(signature);
                        }
                        parts.push(part);
                    }
                }
                if parts.is_empty() {
                    parts.push(json!({ "text": "" }));
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            }
            "tool" => {
                let mut parts = Vec::new();
                while index < messages.len() && messages[index].role == "tool" {
                    let tool_msg = &messages[index];
                    let tool_call_id = tool_msg.tool_call_id.as_deref().unwrap_or("");
                    let tool_name = tool_msg
                        .tool_call_id
                        .as_ref()
                        .and_then(|id| tool_names_by_id.get(id))
                        .cloned()
                        .unwrap_or_else(|| "tool_result".to_string());
                    let mut function_response = json!({
                        "name": tool_name,
                        "response": { "content": tool_msg.content.as_deref().unwrap_or("") }
                    });
                    if !tool_call_id.is_empty() {
                        function_response["id"] = json!(tool_call_id);
                    }
                    parts.push(json!({ "functionResponse": function_response }));
                    index += 1;
                }
                contents.push(json!({ "role": "user", "parts": parts }));
                continue;
            }
            _ => {}
        }
        index += 1;
    }

    let system_instruction = if system_parts.is_empty() {
        None
    } else {
        Some(json!({ "parts": system_parts }))
    };
    Ok((system_instruction, contents))
}

fn gemini_blocking_finish_reason(reason: &str) -> bool {
    matches!(
        reason.trim().to_ascii_uppercase().as_str(),
        "SAFETY"
            | "RECITATION"
            | "LANGUAGE"
            | "BLOCKLIST"
            | "PROHIBITED_CONTENT"
            | "SPII"
            | "IMAGE_SAFETY"
            | "MALFORMED_FUNCTION_CALL"
    )
}

fn gemini_response_error(response: &GeminiGenerateResponse) -> Option<String> {
    if let Some(error) = &response.error {
        let mut details = Vec::new();
        if let Some(code) = error.code {
            details.push(format!("code {code}"));
        }
        if let Some(status) = error.status.as_deref().filter(|status| !status.is_empty()) {
            details.push(status.to_string());
        }
        if let Some(message) = error
            .message
            .as_deref()
            .filter(|message| !message.is_empty())
        {
            details.push(message.to_string());
        }
        let message = if details.is_empty() {
            "unknown error".to_string()
        } else {
            details.join(": ")
        };
        return Some(format!("Gemini API error: {message}"));
    }
    if let Some(reason) = response
        .prompt_feedback
        .as_ref()
        .and_then(|feedback| feedback.block_reason.as_deref())
        .filter(|reason| !reason.is_empty())
    {
        return Some(format!("Gemini blocked the prompt: {reason}"));
    }
    response.candidates.as_deref().and_then(|candidates| {
        candidates.iter().find_map(|candidate| {
            let reason = candidate.finish_reason.as_deref()?;
            gemini_blocking_finish_reason(reason)
                .then(|| format!("Gemini stopped generation with finishReason: {reason}"))
        })
    })
}

fn with_optional_bearer_auth(request: RequestBuilder, api_key: &str) -> RequestBuilder {
    if api_key.is_empty() {
        request
    } else {
        request.bearer_auth(api_key)
    }
}

fn with_optional_gemini_auth(request: RequestBuilder, api_key: &str) -> RequestBuilder {
    if api_key.is_empty() {
        request
    } else {
        request.header("x-goog-api-key", api_key)
    }
}

fn gemini_model_path(model_id: &str) -> String {
    let trimmed = model_id.trim().trim_start_matches('/');
    if trimmed.starts_with("models/") || trimmed.starts_with("publishers/") {
        trimmed.to_string()
    } else {
        format!("models/{trimmed}")
    }
}

fn gemini_generate_url(api_base: &str, model_id: &str, stream: bool) -> String {
    let method = if stream {
        "streamGenerateContent?alt=sse"
    } else {
        "generateContent"
    };
    format!(
        "{}/{}:{}",
        api_base.trim_end_matches('/'),
        gemini_model_path(model_id),
        method
    )
}

fn gemini_usage_pair(usage: Option<&GeminiUsage>) -> (Option<u64>, Option<u64>) {
    let Some(usage) = usage else {
        return (None, None);
    };
    let output = match (usage.candidates_token_count, usage.thoughts_token_count) {
        (Some(candidates), Some(thoughts)) => Some(candidates + thoughts),
        (Some(candidates), None) => Some(candidates),
        (None, Some(thoughts)) => Some(thoughts),
        (None, None) => None,
    };
    (usage.prompt_token_count, output)
}

fn summarize_response_body(body: &str) -> String {
    const MAX_CHARS: usize = 200;

    let trimmed = body.trim();
    let total_chars = trimmed.chars().count();
    if total_chars <= MAX_CHARS {
        return trimmed.to_string();
    }

    let snippet = trimmed.chars().take(MAX_CHARS).collect::<String>();
    format!(
        "{}...\n[truncated at {} chars, total {} chars]",
        snippet, MAX_CHARS, total_chars
    )
}

fn parse_json_response<T: DeserializeOwned>(provider: &str, body: &str) -> Result<T, String> {
    serde_json::from_str(body).map_err(|e| {
        format!(
            "{provider} decode error: {e} - body: {}",
            summarize_response_body(body)
        )
    })
}

fn provider_json_error(provider: &str, data: &Value) -> Option<String> {
    let error = data.get("error")?;
    let detail = match error {
        Value::String(message) => message.trim().to_string(),
        Value::Object(obj) => {
            let mut parts = Vec::new();
            for key in ["code", "status", "type", "message"] {
                let Some(value) = obj.get(key) else {
                    continue;
                };
                let text = match value {
                    Value::String(text) => text.trim().to_string(),
                    Value::Number(number) => number.to_string(),
                    _ => String::new(),
                };
                if !text.is_empty() {
                    parts.push(text);
                }
            }
            if parts.is_empty() {
                error.to_string()
            } else {
                parts.join(": ")
            }
        }
        _ => error.to_string(),
    };
    if detail.is_empty() {
        Some(format!("{provider} API error: unknown error"))
    } else {
        Some(format!("{provider} API error: {detail}"))
    }
}

fn response_content_type(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase())
}

fn is_html_response_content_type(content_type: &str) -> bool {
    content_type.starts_with("text/html") || content_type.starts_with("application/xhtml+xml")
}

async fn validate_stream_response(
    provider: &str,
    expected_stream: &str,
    resp: reqwest::Response,
) -> Result<reqwest::Response, String> {
    let Some(content_type) = response_content_type(&resp) else {
        return Ok(resp);
    };

    if is_html_response_content_type(&content_type) {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "{provider} stream error: expected {expected_stream}, got {content_type} - body: {}",
            summarize_response_body(&body)
        ));
    }

    if content_type.starts_with("application/json") {
        let body = resp.text().await.unwrap_or_default();
        if let Ok(data) = serde_json::from_str::<Value>(&body)
            && let Some(error) = provider_json_error(provider, &data)
        {
            return Err(error);
        }
        return Err(format!(
            "{provider} stream error: expected {expected_stream}, got {content_type} - body: {}",
            summarize_response_body(&body)
        ));
    }

    Ok(resp)
}

fn ollama_request_options(resolved: &ResolvedModel) -> serde_json::Value {
    let mut options = serde_json::Map::new();
    options.insert("num_ctx".to_string(), json!(resolved.context_window));
    if let Some(max_tokens) = resolved.max_tokens {
        options.insert("num_predict".to_string(), json!(max_tokens));
    }
    serde_json::Value::Object(options)
}

fn build_openai_simple_body(
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    s3_cfg: Option<&crate::config::S3Config>,
) -> Result<serde_json::Value, String> {
    let messages = materialize_image_urls(messages, s3_cfg)?;
    let prepared_messages = prepare_openai_messages_for_request(
        resolved,
        &messages,
        resolved.thinking_format.as_deref() == Some("deepseek-v4"),
    );
    let api_messages = convert_messages_to_openai_with_options(
        prepared_messages.as_ref(),
        openai_prefers_null_tool_call_content(resolved),
        resolved.thinking_format.as_deref(),
    );
    let mut body = json!({
        "model": resolved.model_id,
        "messages": api_messages,
    });
    if let Some(mt) = resolved.max_tokens {
        body["max_tokens"] = json!(mt);
    }
    Ok(body)
}

async fn build_gemini_body(
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    workspace: &Path,
    s3_cfg: Option<&crate::config::S3Config>,
    extra_tools: &[serde_json::Value],
    include_builtin_tools: bool,
    think_level: &str,
) -> Result<serde_json::Value, String> {
    let messages = materialize_image_urls(messages, s3_cfg)?;
    let images_b64 = fetch_images_base64(&messages, workspace, s3_cfg, true).await?;
    let (system_instruction, contents) = convert_messages_to_gemini(&messages, &images_b64)?;
    let mut all_tools: Vec<serde_json::Value> = if include_builtin_tools {
        serde_json::from_value(tools::tool_definitions_gemini()).unwrap_or_default()
    } else {
        Vec::new()
    };
    all_tools.extend_from_slice(extra_tools);
    let mut body = json!({
        "contents": contents,
    });
    if let Some(system_instruction) = system_instruction {
        body["systemInstruction"] = system_instruction;
    }
    if !all_tools.is_empty() {
        body["tools"] = json!([{ "functionDeclarations": all_tools }]);
    }
    let mut generation_config = serde_json::Map::new();
    if let Some(max_tokens) = resolved.max_tokens {
        generation_config.insert("maxOutputTokens".to_string(), json!(max_tokens));
    }
    if gemini_uses_thinking_level(resolved) {
        generation_config.insert(
            "thinkingConfig".to_string(),
            json!({
                "includeThoughts": think_level != "off",
                "thinkingLevel": think_level_to_gemini_thinking_level(think_level),
            }),
        );
    }
    if !generation_config.is_empty() {
        body["generationConfig"] = Value::Object(generation_config);
    }
    Ok(body)
}

// ══════════════════════════════════════════════════════════════════════════════
//  LLM Streaming Client
// ══════════════════════════════════════════════════════════════════════════════

/// Non-streaming LLM call — returns plain text plus optional usage counters.
pub(crate) async fn call_llm_simple_with_usage(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    workspace: &Path,
    s3_cfg: Option<&crate::config::S3Config>,
    max_retries: usize,
) -> Result<SimpleLlmResponse, String> {
    match resolved.provider {
        Provider::OpenAI => {
            let url = format!("{}/chat/completions", resolved.api_base);
            let body = build_openai_simple_body(resolved, messages, s3_cfg)?;
            let resp = send_with_retry(http, max_retries, || {
                http.post(&url).bearer_auth(&resolved.api_key).json(&body)
            })
            .await?;
            let text = resp.text().await.map_err(|e| e.to_string())?;
            let data: serde_json::Value = parse_json_response("OpenAI", &text)?;
            if let Some(error) = provider_json_error("OpenAI", &data) {
                return Err(error);
            }
            Ok(SimpleLlmResponse {
                content: data["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
                input_tokens: data["usage"]["prompt_tokens"].as_u64(),
                output_tokens: data["usage"]["completion_tokens"].as_u64(),
            })
        }
        Provider::Anthropic => {
            let url = format!("{}/v1/messages", resolved.api_base);
            let messages = materialize_image_urls(messages, s3_cfg)?;
            let (system, msgs) = convert_messages_to_anthropic(&messages);
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
            let resp = send_with_retry(http, max_retries, || {
                http.post(&url)
                    .header("x-api-key", &resolved.api_key)
                    .header("anthropic-version", "2023-06-01")
                    .json(&body)
            })
            .await?;
            let text = resp.text().await.map_err(|e| e.to_string())?;
            let data: serde_json::Value = parse_json_response("Anthropic", &text)?;
            if let Some(error) = provider_json_error("Anthropic", &data) {
                return Err(error);
            }
            let content = data["content"]
                .as_array()
                .and_then(|arr| arr.iter().find(|b| b["type"] == "text"))
                .and_then(|b| b["text"].as_str())
                .unwrap_or("")
                .to_string();
            Ok(SimpleLlmResponse {
                content,
                input_tokens: data["usage"]["input_tokens"].as_u64(),
                output_tokens: data["usage"]["output_tokens"].as_u64(),
            })
        }
        Provider::Ollama => {
            let url = format!("{}/api/chat", resolved.api_base);
            let messages = materialize_image_urls(messages, s3_cfg)?;
            let images_b64 = fetch_images_base64(&messages, workspace, s3_cfg, false).await?;
            let api_messages = convert_messages_to_ollama(&messages, &images_b64);
            let mut body = json!({
                "model": resolved.model_id,
                "messages": api_messages,
                "stream": false,
            });
            body["options"] = ollama_request_options(resolved);
            let resp = send_with_retry(http, max_retries, || {
                with_optional_bearer_auth(http.post(&url), &resolved.api_key).json(&body)
            })
            .await?;
            let text = resp.text().await.map_err(|e| e.to_string())?;
            let data: serde_json::Value = parse_json_response("Ollama", &text)?;
            if let Some(error) = provider_json_error("Ollama", &data) {
                return Err(error);
            }
            Ok(SimpleLlmResponse {
                content: data["message"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
                input_tokens: data["prompt_eval_count"].as_u64(),
                output_tokens: data["eval_count"].as_u64(),
            })
        }
        Provider::Gemini => {
            let url = gemini_generate_url(&resolved.api_base, &resolved.model_id, false);
            let body =
                build_gemini_body(resolved, messages, workspace, s3_cfg, &[], false, "off").await?;
            let resp = send_with_retry(http, max_retries, || {
                with_optional_gemini_auth(http.post(&url), &resolved.api_key).json(&body)
            })
            .await?;
            let text = resp.text().await.map_err(|e| e.to_string())?;
            let data: GeminiGenerateResponse = parse_json_response("Gemini", &text)?;
            if let Some(error) = gemini_response_error(&data) {
                return Err(error);
            }
            let content = data
                .candidates
                .as_deref()
                .and_then(|candidates| candidates.first())
                .and_then(|candidate| candidate.content.as_ref())
                .and_then(|content| content.parts.as_ref())
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part.text.as_deref())
                        .collect::<String>()
                })
                .unwrap_or_default();
            let (input_tokens, output_tokens) = gemini_usage_pair(data.usage_metadata.as_ref());
            Ok(SimpleLlmResponse {
                content,
                input_tokens,
                output_tokens,
            })
        }
    }
}

/// Backward-compatible non-streaming helper when the caller only needs text.
pub(crate) async fn call_llm_simple(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    workspace: &Path,
    s3_cfg: Option<&crate::config::S3Config>,
    max_retries: usize,
) -> Result<String, String> {
    call_llm_simple_with_usage(http, resolved, messages, workspace, s3_cfg, max_retries)
        .await
        .map(|resp| resp.content)
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

/// Map think_level to DeepSeek reasoning_effort string.
/// DeepSeek-v4 only supports "high" and "max"; lower levels map to "high".
fn think_level_to_deepseek_reasoning_effort(level: &str) -> &str {
    match level {
        "xhigh" => "max",
        _ => "high",
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

fn think_level_to_gemini_thinking_level(level: &str) -> &'static str {
    match level {
        "off" | "minimal" => "MINIMAL",
        "low" => "LOW",
        "medium" => "MEDIUM",
        "high" | "xhigh" => "HIGH",
        _ => "HIGH",
    }
}

pub(crate) fn gemini_uses_thinking_level(resolved: &ResolvedModel) -> bool {
    resolved
        .model_id
        .trim()
        .to_ascii_lowercase()
        .contains("gemini-3")
}

pub(crate) fn auto_think_supported(resolved: &ResolvedModel) -> bool {
    match resolved.provider {
        Provider::OpenAI => resolved.reasoning && openai_supports_reasoning_controls(resolved),
        Provider::Anthropic => resolved.reasoning,
        Provider::Ollama => resolved.reasoning || resolved.thinking_format.is_some(),
        Provider::Gemini => {
            resolved.reasoning
                || resolved.thinking_format.is_some()
                || gemini_uses_thinking_level(resolved)
        }
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

/// Delimiter that separates the stable system-prompt prefix from the per-round
/// dynamic suffix.  Must match the template in `build_system_prompt_with_query`.
const ENV_BLOCK_DELIMITER: &str = "\n\n---\n## Environment\n- OS:";

fn anthropic_system_payload(system_prompt: &str, cache_enabled: bool) -> serde_json::Value {
    if !cache_enabled {
        return json!(system_prompt);
    }
    let split_pos = system_prompt.rfind(ENV_BLOCK_DELIMITER);
    let (stable, dynamic) = match split_pos {
        Some(pos) => (&system_prompt[..pos], Some(&system_prompt[pos..])),
        None => (system_prompt, None),
    };
    let mut blocks: Vec<serde_json::Value> = vec![json!({
        "type": "text",
        "text": stable,
        "cache_control": {"type": "ephemeral"},
    })];
    if let Some(dyn_text) = dynamic {
        blocks.push(json!({"type": "text", "text": dyn_text}));
    }
    json!(blocks)
}

fn maybe_apply_anthropic_tool_cache_control(tools: &mut [serde_json::Value], cache_enabled: bool) {
    if !cache_enabled {
        return;
    }
    if let Some(last_tool) = tools.last_mut() {
        last_tool["cache_control"] = json!({"type": "ephemeral"});
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn call_llm_stream(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    workspace: &Path,
    s3_cfg: Option<&crate::config::S3Config>,
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
    max_retries: usize,
) -> Result<LlmResponse, String> {
    call_llm_stream_with_tool_mode(
        http,
        resolved,
        messages,
        workspace,
        s3_cfg,
        tx,
        think_level,
        extra_tools,
        true,
        max_retries,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn call_llm_stream_with_tool_mode(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    workspace: &Path,
    s3_cfg: Option<&crate::config::S3Config>,
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
    include_builtin_tools: bool,
    max_retries: usize,
) -> Result<LlmResponse, String> {
    // Resolve "auto": enable thinking at medium level if model supports it, else off
    let effective_level = if think_level == "auto" {
        if auto_think_supported(resolved) {
            "medium"
        } else {
            "off"
        }
    } else {
        think_level
    };
    match resolved.provider {
        Provider::OpenAI => {
            call_llm_stream_openai(
                http,
                resolved,
                messages,
                s3_cfg,
                tx,
                effective_level,
                extra_tools,
                include_builtin_tools,
                max_retries,
            )
            .await
        }
        Provider::Anthropic => {
            call_llm_stream_anthropic(
                http,
                resolved,
                messages,
                s3_cfg,
                tx,
                effective_level,
                extra_tools,
                include_builtin_tools,
                max_retries,
            )
            .await
        }
        Provider::Ollama => {
            call_llm_stream_ollama(
                http,
                resolved,
                messages,
                workspace,
                s3_cfg,
                tx,
                effective_level,
                extra_tools,
                include_builtin_tools,
                max_retries,
            )
            .await
        }
        Provider::Gemini => {
            call_llm_stream_gemini(
                http,
                resolved,
                messages,
                workspace,
                s3_cfg,
                tx,
                effective_level,
                extra_tools,
                include_builtin_tools,
                max_retries,
            )
            .await
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
                        state.thinking_buf.push_str(think_text);
                        state.client_gone =
                            !live_send(tx, json!({"type":"thinking_delta","content":think_text}))
                                .await;
                    }
                }
                let has_content = choice
                    .delta
                    .content
                    .as_ref()
                    .is_some_and(|text| !text.is_empty());
                let has_tool_calls = choice
                    .delta
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty());

                if (has_content || has_tool_calls) && state.reasoning_started && !state.client_gone
                {
                    state.reasoning_started = false;
                    state.client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
                }

                if let Some(text) = &choice.delta.content
                    && !text.is_empty()
                {
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
                                gemini_thought_signature: None,
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
                            let thinking_idx = state.thinking_blocks.len();
                            let initial_thinking = block.thinking.clone().unwrap_or_default();
                            state.thinking_blocks.push(AnthropicThinkingBlock {
                                block_type: "thinking".into(),
                                thinking: Some(initial_thinking.clone()),
                                signature: block.signature.clone(),
                                data: None,
                            });
                            if let Some(block_idx) = evt.index {
                                state.block_thinking_idx.insert(block_idx, thinking_idx);
                                state.thinking_block_idx = Some(block_idx);
                            }
                            if !state.client_gone {
                                state.reasoning_started = true;
                                state.client_gone =
                                    !live_send(tx, json!({"type":"thinking_start"})).await;
                                if !state.client_gone && !initial_thinking.is_empty() {
                                    state.thinking_buf.push_str(&initial_thinking);
                                    state.client_gone = !live_send(
                                        tx,
                                        json!({"type":"thinking_delta","content":initial_thinking}),
                                    )
                                    .await;
                                }
                            }
                        }
                        "redacted_thinking" => {
                            state.thinking_blocks.push(AnthropicThinkingBlock {
                                block_type: "redacted_thinking".into(),
                                thinking: None,
                                signature: None,
                                data: block.data.clone(),
                            });
                        }
                        "tool_use" => {
                            let idx = state.tool_calls.len();
                            state.tool_calls.push(ToolCall {
                                id: block.id.clone().unwrap_or_default(),
                                call_type: "function".into(),
                                gemini_thought_signature: None,
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
                            if let Some(text) = &delta.thinking {
                                if let Some(block_idx) = evt.index
                                    && let Some(&thinking_idx) =
                                        state.block_thinking_idx.get(&block_idx)
                                    && let Some(block) = state.thinking_blocks.get_mut(thinking_idx)
                                {
                                    block
                                        .thinking
                                        .get_or_insert_with(String::new)
                                        .push_str(text);
                                }
                                if !text.is_empty() {
                                    state.thinking_buf.push_str(text);
                                    if !state.client_gone {
                                        state.client_gone = !live_send(
                                            tx,
                                            json!({"type":"thinking_delta","content":text}),
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                        Some("signature_delta") => {
                            if let Some(signature) = &delta.signature
                                && let Some(block_idx) = evt.index
                                && let Some(&thinking_idx) =
                                    state.block_thinking_idx.get(&block_idx)
                                && let Some(block) = state.thinking_blocks.get_mut(thinking_idx)
                            {
                                block.signature = Some(signature.clone());
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
            state.thinking_buf.push_str(thinking);
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
                    gemini_thought_signature: None,
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

async fn process_gemini_data_line(
    data: &str,
    tx: &LiveTx,
    state: &mut OpenAiStreamState,
) -> Result<(), String> {
    let Ok(chunk) = serde_json::from_str::<GeminiGenerateResponse>(data) else {
        return Ok(());
    };

    if let Some(error) = gemini_response_error(&chunk) {
        return Err(error);
    }

    let (input_tokens, output_tokens) = gemini_usage_pair(chunk.usage_metadata.as_ref());
    if let Some(value) = input_tokens {
        state.input_tokens = Some(value);
    }
    if let Some(value) = output_tokens {
        state.output_tokens = Some(value);
    }

    let Some(candidates) = chunk.candidates else {
        return Ok(());
    };
    for candidate in candidates {
        let Some(parts) = candidate.content.and_then(|content| content.parts) else {
            continue;
        };
        for part in parts {
            if let Some(text) = part.text
                && !text.is_empty()
            {
                if part.thought.unwrap_or(false) {
                    if !state.reasoning_started && !state.client_gone {
                        state.reasoning_started = true;
                        state.client_gone = !live_send(tx, json!({"type":"thinking_start"})).await;
                    }
                    state.thinking_buf.push_str(&text);
                    if !state.client_gone
                        && !live_send(tx, json!({"type":"thinking_delta","content":text})).await
                    {
                        state.client_gone = true;
                    }
                } else {
                    if state.reasoning_started && !state.client_gone {
                        state.reasoning_started = false;
                        state.client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
                    }
                    state.content_buf.push_str(&text);
                    if !state.client_gone
                        && !live_send(tx, json!({"type":"delta","content":text})).await
                    {
                        state.client_gone = true;
                    }
                }
            }
            if let Some(function_call) = part.function_call {
                if state.reasoning_started && !state.client_gone {
                    state.reasoning_started = false;
                    state.client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
                }
                let idx = state.tool_calls.len();
                let arguments = function_call.args.unwrap_or_else(|| json!({}));
                state.tool_calls.push(ToolCall {
                    id: function_call
                        .id
                        .filter(|id| !id.trim().is_empty())
                        .unwrap_or_else(|| format!("gemini_call_{}", idx + 1)),
                    call_type: "function".into(),
                    gemini_thought_signature: part.thought_signature,
                    function: FunctionCall {
                        name: function_call.name.unwrap_or_default(),
                        arguments: serde_json::to_string(&arguments)
                            .unwrap_or_else(|_| "{}".to_string()),
                    },
                });
            }
        }
    }
    Ok(())
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
    s3_cfg: Option<&crate::config::S3Config>,
    think_level: &str,
    extra_tools: &[serde_json::Value],
    include_builtin_tools: bool,
) -> Result<serde_json::Value, String> {
    let thinking_on = think_level != "off";
    let messages = materialize_image_urls(messages, s3_cfg)?;
    let prepared_messages = prepare_openai_messages_for_request(resolved, &messages, thinking_on);
    let api_messages = convert_messages_to_openai_with_options(
        prepared_messages.as_ref(),
        openai_prefers_null_tool_call_content(resolved),
        resolved.thinking_format.as_deref(),
    );
    let mut all_tools: Vec<serde_json::Value> = if include_builtin_tools {
        serde_json::from_value(tools::tool_definitions()).unwrap_or_default()
    } else {
        Vec::new()
    };
    all_tools.extend_from_slice(extra_tools);
    let mut body = json!({
        "model": resolved.model_id,
        "messages": api_messages,
        "tools": all_tools,
        "stream": true,
    });
    if resolved.provider == Provider::OpenAI
        && (is_official_openai_api_base(&resolved.api_base) || resolved.stream_include_usage)
    {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if thinking_on {
        match resolved.thinking_format.as_deref().unwrap_or("openai") {
            "qwen" => {
                body["enable_thinking"] = json!(true);
            }
            "deepseek-v4" => {
                body["reasoning_effort"] =
                    json!(think_level_to_deepseek_reasoning_effort(think_level));
                body["thinking"] = json!({"type": "enabled"});
            }
            _ => {
                body["reasoning_effort"] = json!(think_level_to_reasoning_effort(think_level));
            }
        }
    } else if resolved.thinking_format.as_deref() == Some("deepseek-v4") {
        // DeepSeek defaults to thinking enabled; explicitly disable it.
        body["thinking"] = json!({"type": "disabled"});
    }
    if let Some(max_tokens) = resolved.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }
    Ok(body)
}

async fn build_ollama_stream_body(
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    workspace: &Path,
    s3_cfg: Option<&crate::config::S3Config>,
    think_level: &str,
    extra_tools: &[serde_json::Value],
    include_builtin_tools: bool,
) -> Result<serde_json::Value, String> {
    let messages = materialize_image_urls(messages, s3_cfg)?;
    let images_b64 = fetch_images_base64(&messages, workspace, s3_cfg, false).await?;
    let api_messages = convert_messages_to_ollama(&messages, &images_b64);
    let mut all_tools: Vec<serde_json::Value> = if include_builtin_tools {
        serde_json::from_value(tools::tool_definitions_ollama()).unwrap_or_default()
    } else {
        Vec::new()
    };
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
    Ok(body)
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
    s3_cfg: Option<&crate::config::S3Config>,
    think_level: &str,
    extra_tools: &[serde_json::Value],
    include_builtin_tools: bool,
) -> Result<serde_json::Value, String> {
    let thinking_on = think_level != "off";
    let messages = materialize_image_urls(messages, s3_cfg)?;
    let (system_prompt, anthropic_msgs) = convert_messages_to_anthropic(&messages);
    let base_max = resolved.max_tokens.unwrap_or(8192);
    let mut all_tools: Vec<serde_json::Value> = if include_builtin_tools {
        serde_json::from_value(tools::tool_definitions_anthropic()).unwrap_or_default()
    } else {
        Vec::new()
    };
    all_tools.extend_from_slice(extra_tools);
    let cache_enabled = anthropic_prompt_caching_enabled(resolved);
    maybe_apply_anthropic_tool_cache_control(&mut all_tools, cache_enabled);
    let mut body = json!({
        "model": resolved.model_id,
        "messages": anthropic_msgs,
        "tools": all_tools,
        "max_tokens": base_max,
        "stream": true,
    });
    if thinking_on {
        // Anthropic requires: 1024 <= budget_tokens < max_tokens.
        // Clamp to leave at least 1024 tokens for actual text output.
        let raw_budget = think_level_to_budget(think_level);
        let budget_tokens = raw_budget.min(base_max.saturating_sub(1024));
        if budget_tokens >= 1024 {
            body["thinking"] = json!({
                "type": "enabled",
                "budget_tokens": budget_tokens,
            });
        }
    }
    if !system_prompt.is_empty() {
        body["system"] = anthropic_system_payload(&system_prompt, cache_enabled);
    }
    Ok(body)
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

async fn consume_gemini_stream<S, B>(
    stream: &mut S,
    tx: &LiveTx,
    state: &mut OpenAiStreamState,
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
            let line = line.trim();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(data) = line.strip_prefix("data: ") {
                process_gemini_data_line(data.trim(), tx, state).await?;
            }
        }
    }

    let trailing = partial_buf.trim();
    if let Some(data) = trailing.strip_prefix("data: ") {
        process_gemini_data_line(data.trim(), tx, state).await?;
    }

    Ok(())
}

/// Send an HTTP request with automatic retry for transient failures.
/// Retries on 429 (rate limit), 5xx (server error), and connection/timeout errors.
/// Uses exponential backoff (1s, 2s, 4s, …) but respects `Retry-After` header for 429.
async fn send_with_retry(
    _http: &Client,
    max_retries: usize,
    mut build: impl FnMut() -> reqwest::RequestBuilder,
) -> Result<reqwest::Response, String> {
    for attempt in 0..=max_retries {
        let response = build().send().await;
        match response {
            Ok(resp) => {
                let status = resp.status();
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    if attempt < max_retries {
                        // Respect Retry-After header on 429, cap at 30s.
                        let retry_after_secs = resp
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(|s| s.parse::<u64>().ok())
                            .map(|s| s.min(30));
                        let delay = if let Some(secs) = retry_after_secs {
                            Duration::from_secs(secs)
                        } else {
                            Duration::from_millis(1000 * (1u64 << attempt.min(6)))
                        };
                        eprintln!(
                            "LLM API {status}, retrying ({}/{max_retries}){}",
                            attempt + 1,
                            retry_after_secs
                                .map(|s| format!(" [Retry-After: {s}s]"))
                                .unwrap_or_default(),
                        );
                        tokio::time::sleep(delay).await;
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
            Err(e) if attempt < max_retries && (e.is_connect() || e.is_timeout()) => {
                let delay = Duration::from_millis(1000 * (1u64 << attempt.min(6)));
                eprintln!(
                    "LLM request error: {e}, retrying ({}/{max_retries})",
                    attempt + 1,
                );
                tokio::time::sleep(delay).await;
                continue;
            }
            Err(e) => return Err(format!("HTTP error: {e}")),
        }
    }
    Err("LLM request failed after all retries".into())
}

/// Returns `true` if an LLM error string represents a transient failure where
/// no partial streaming content was sent to the client – safe to retry at the
/// agent level.
pub(crate) fn is_transient_llm_error(error: &str) -> bool {
    // Stream-phase errors: partial content may have been sent to the client.
    if error.starts_with("stream error:") {
        return false;
    }
    // Client disconnected: no point retrying.
    if error == "Client disconnected" {
        return false;
    }
    // HTTP-level transient errors produced by send_with_retry():
    //   "API 429 …" / "API 500 …" / "API 502 …" / "API 503 …" / "API 504 …"
    //   "HTTP error: …" (connection / timeout)
    //   "LLM request failed after all retries"
    if error.starts_with("HTTP error:") || error.starts_with("LLM request failed") {
        return true;
    }
    if let Some(rest) = error.strip_prefix("API ") {
        return rest.starts_with("429")
            || rest.starts_with("500")
            || rest.starts_with("502")
            || rest.starts_with("503")
            || rest.starts_with("504");
    }
    false
}

#[allow(clippy::too_many_arguments)]
async fn call_llm_stream_openai(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    s3_cfg: Option<&crate::config::S3Config>,
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
    include_builtin_tools: bool,
    max_retries: usize,
) -> Result<LlmResponse, String> {
    let url = format!("{}/chat/completions", resolved.api_base);
    let body = build_openai_stream_body(
        resolved,
        messages,
        s3_cfg,
        think_level,
        extra_tools,
        include_builtin_tools,
    )?;

    let resp = send_with_retry(http, max_retries, || {
        http.post(&url).bearer_auth(&resolved.api_key).json(&body)
    })
    .await?;
    let resp = validate_stream_response("OpenAI", "SSE", resp).await?;

    let mut stream = resp.bytes_stream();
    let mut stream_state = OpenAiStreamState {
        content_buf: String::new(),
        thinking_buf: String::new(),
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
        stream_state.thinking_buf,
        stream_state.tool_calls,
        stream_state.input_tokens,
        stream_state.output_tokens,
    )
}

#[allow(clippy::too_many_arguments)]
async fn call_llm_stream_anthropic(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    s3_cfg: Option<&crate::config::S3Config>,
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
    include_builtin_tools: bool,
    max_retries: usize,
) -> Result<LlmResponse, String> {
    let url = format!("{}/v1/messages", resolved.api_base);
    let body = build_anthropic_stream_body(
        resolved,
        messages,
        s3_cfg,
        think_level,
        extra_tools,
        include_builtin_tools,
    )?;

    let resp = send_with_retry(http, max_retries, || {
        http.post(&url)
            .header("x-api-key", &resolved.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
    })
    .await?;
    let resp = validate_stream_response("Anthropic", "SSE", resp).await?;

    let mut stream = resp.bytes_stream();
    let mut stream_state = AnthropicStreamState {
        current_event_type: String::new(),
        content_buf: String::new(),
        thinking_buf: String::new(),
        thinking_blocks: Vec::new(),
        tool_calls: Vec::new(),
        input_tokens: None,
        output_tokens: None,
        block_tool_idx: HashMap::new(),
        block_thinking_idx: HashMap::new(),
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

    build_anthropic_llm_response(
        stream_state.content_buf,
        stream_state.thinking_buf,
        stream_state.thinking_blocks,
        stream_state.tool_calls,
        stream_state.input_tokens,
        stream_state.output_tokens,
    )
}

#[allow(clippy::too_many_arguments)]
async fn call_llm_stream_ollama(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    workspace: &Path,
    s3_cfg: Option<&crate::config::S3Config>,
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
    include_builtin_tools: bool,
    max_retries: usize,
) -> Result<LlmResponse, String> {
    let url = format!("{}/api/chat", resolved.api_base);
    let body = build_ollama_stream_body(
        resolved,
        messages,
        workspace,
        s3_cfg,
        think_level,
        extra_tools,
        include_builtin_tools,
    )
    .await?;

    let resp = send_with_retry(http, max_retries, || {
        with_optional_bearer_auth(http.post(&url), &resolved.api_key).json(&body)
    })
    .await?;
    let resp = validate_stream_response("Ollama", "NDJSON", resp).await?;

    let mut stream = resp.bytes_stream();
    let mut stream_state = OpenAiStreamState {
        content_buf: String::new(),
        thinking_buf: String::new(),
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
        stream_state.thinking_buf,
        stream_state.tool_calls,
        stream_state.input_tokens,
        stream_state.output_tokens,
    )
}

#[allow(clippy::too_many_arguments)]
async fn call_llm_stream_gemini(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    workspace: &Path,
    s3_cfg: Option<&crate::config::S3Config>,
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
    include_builtin_tools: bool,
    max_retries: usize,
) -> Result<LlmResponse, String> {
    let url = gemini_generate_url(&resolved.api_base, &resolved.model_id, true);
    let body = build_gemini_body(
        resolved,
        messages,
        workspace,
        s3_cfg,
        extra_tools,
        include_builtin_tools,
        think_level,
    )
    .await?;

    let resp = send_with_retry(http, max_retries, || {
        with_optional_gemini_auth(http.post(&url), &resolved.api_key).json(&body)
    })
    .await?;
    let resp = validate_stream_response("Gemini", "SSE", resp).await?;

    let mut stream = resp.bytes_stream();
    let mut stream_state = OpenAiStreamState {
        content_buf: String::new(),
        thinking_buf: String::new(),
        tool_calls: Vec::new(),
        input_tokens: None,
        output_tokens: None,
        client_gone: false,
        reasoning_started: false,
    };
    consume_gemini_stream(&mut stream, tx, &mut stream_state).await?;

    if stream_state.reasoning_started && !stream_state.client_gone {
        stream_state.client_gone = !live_send(tx, json!({"type":"thinking_done"})).await;
    }
    if stream_state.client_gone {
        return Err("Client disconnected".into());
    }

    build_llm_response(
        stream_state.content_buf,
        stream_state.thinking_buf,
        stream_state.tool_calls,
        stream_state.input_tokens,
        stream_state.output_tokens,
    )
}

fn normalized_anthropic_thinking_blocks(
    blocks: Vec<AnthropicThinkingBlock>,
) -> Option<Vec<AnthropicThinkingBlock>> {
    let blocks = blocks
        .into_iter()
        .filter(|block| match block.block_type.as_str() {
            "thinking" => block
                .signature
                .as_deref()
                .is_some_and(|signature| !signature.is_empty()),
            "redacted_thinking" => block.data.as_deref().is_some_and(|data| !data.is_empty()),
            _ => false,
        })
        .collect::<Vec<_>>();
    if blocks.is_empty() {
        None
    } else {
        Some(blocks)
    }
}

fn build_anthropic_llm_response(
    content_buf: String,
    thinking_buf: String,
    thinking_blocks: Vec<AnthropicThinkingBlock>,
    tool_calls: Vec<ToolCall>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Result<LlmResponse, String> {
    let mut response = build_llm_response(
        content_buf,
        thinking_buf,
        tool_calls,
        input_tokens,
        output_tokens,
    )?;
    response.message.anthropic_thinking_blocks =
        normalized_anthropic_thinking_blocks(thinking_blocks);
    Ok(response)
}

fn build_llm_response(
    content_buf: String,
    thinking_buf: String,
    tool_calls: Vec<ToolCall>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
) -> Result<LlmResponse, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let tc = if tool_calls.is_empty() {
        None
    } else {
        Some(normalize_tool_call_ids(tool_calls, now.as_nanos()))
    };
    let content = if content_buf.is_empty() {
        None
    } else {
        Some(content_buf)
    };
    let thinking = if thinking_buf.is_empty() {
        None
    } else {
        Some(thinking_buf)
    };

    Ok(LlmResponse {
        message: ChatMessage {
            role: "assistant".into(),
            content,
            images: None,
            thinking,
            anthropic_thinking_blocks: None,
            tool_calls: tc,
            tool_call_id: None,
            timestamp: Some(now.as_secs()),
        },
        input_tokens,
        output_tokens,
    })
}

fn normalize_tool_call_ids(mut tool_calls: Vec<ToolCall>, seed: u128) -> Vec<ToolCall> {
    let mut seen = HashSet::new();

    for (idx, tool_call) in tool_calls.iter_mut().enumerate() {
        let needs_fallback = tool_call.id.trim().is_empty() || seen.contains(&tool_call.id);
        if needs_fallback {
            tool_call.id = format!("tool_call_{seed}_{}", idx + 1);
        }
        seen.insert(tool_call.id.clone());
    }

    tool_calls
}

fn total_anthropic_input_tokens(usage: &AnthropicUsage) -> u64 {
    usage.input_tokens.unwrap_or(0)
        + usage.cache_creation_input_tokens.unwrap_or(0)
        + usage.cache_read_input_tokens.unwrap_or(0)
}

#[cfg(test)]
#[path = "tests/providers_tests.rs"]
mod tests;
