use std::{
    collections::HashMap,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::{Stream, StreamExt};
use reqwest::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::{Value, json};

use base64::Engine;

use crate::{
    ChatMessage, FunctionCall, LiveTx, Provider, ToolCall, image_uploads, live_send, tools,
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
            "system" => {
                out.push(json!({
                    "role": "system",
                    "content": msg.content.as_deref().unwrap_or(""),
                }));
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
/// Individual image fetch failures are logged as warnings and skipped;
/// only client construction failure returns `Err`.
async fn fetch_images_base64(
    messages: &[ChatMessage],
    workspace: &Path,
    s3_cfg: Option<&crate::config::S3Config>,
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
            let messages = materialize_image_urls(messages, s3_cfg)?;
            let api_messages = convert_messages_to_openai(&messages);
            let mut body = json!({
                "model": resolved.model_id,
                "messages": api_messages,
            });
            if let Some(mt) = resolved.max_tokens {
                body["max_tokens"] = json!(mt);
            }
            let resp = send_with_retry(http, max_retries, || {
                http.post(&url).bearer_auth(&resolved.api_key).json(&body)
            })
            .await?;
            let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
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
            let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
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
            let images_b64 = fetch_images_base64(&messages, workspace, s3_cfg).await?;
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
            let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            Ok(SimpleLlmResponse {
                content: data["message"]["content"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
                input_tokens: data["prompt_eval_count"].as_u64(),
                output_tokens: data["eval_count"].as_u64(),
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
            call_llm_stream_openai(
                http,
                resolved,
                messages,
                s3_cfg,
                tx,
                effective_level,
                extra_tools,
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
    s3_cfg: Option<&crate::config::S3Config>,
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let thinking_on = think_level != "off";
    let messages = materialize_image_urls(messages, s3_cfg)?;
    let api_messages = convert_messages_to_openai(&messages);
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
    Ok(body)
}

async fn build_ollama_stream_body(
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    workspace: &Path,
    s3_cfg: Option<&crate::config::S3Config>,
    think_level: &str,
    extra_tools: &[serde_json::Value],
) -> Result<serde_json::Value, String> {
    let messages = materialize_image_urls(messages, s3_cfg)?;
    let images_b64 = fetch_images_base64(&messages, workspace, s3_cfg).await?;
    let api_messages = convert_messages_to_ollama(&messages, &images_b64);
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
) -> Result<serde_json::Value, String> {
    let thinking_on = think_level != "off";
    let messages = materialize_image_urls(messages, s3_cfg)?;
    let (system_prompt, anthropic_msgs) = convert_messages_to_anthropic(&messages);
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
    max_retries: usize,
) -> Result<LlmResponse, String> {
    let url = format!("{}/chat/completions", resolved.api_base);
    let body = build_openai_stream_body(resolved, messages, s3_cfg, think_level, extra_tools)?;

    let resp = send_with_retry(http, max_retries, || {
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

#[allow(clippy::too_many_arguments)]
async fn call_llm_stream_anthropic(
    http: &Client,
    resolved: &ResolvedModel,
    messages: &[ChatMessage],
    s3_cfg: Option<&crate::config::S3Config>,
    tx: &LiveTx,
    think_level: &str,
    extra_tools: &[serde_json::Value],
    max_retries: usize,
) -> Result<LlmResponse, String> {
    let url = format!("{}/v1/messages", resolved.api_base);
    let body = build_anthropic_stream_body(resolved, messages, s3_cfg, think_level, extra_tools)?;

    let resp = send_with_retry(http, max_retries, || {
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
    )
    .await?;

    let resp = send_with_retry(http, max_retries, || {
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
            images: None,
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
