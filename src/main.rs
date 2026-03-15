use axum::{
    extract::{
        ws::{Message as WsMsg, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use futures::{stream::SplitSink, SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tower_http::{cors::CorsLayer, services::ServeDir};

mod cli;
mod prompts;
mod providers;
mod tools;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

// ══════════════════════════════════════════════════════════════════════════════
//  Config
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provider {
    OpenAI,
    Anthropic,
}

impl Provider {
    fn detect(model: &str, api_base: &str, json_provider: Option<&str>) -> Self {
        // Explicit override: env var > JSON settings > auto-detect
        let env_explicit = std::env::var("LINGCLAW_PROVIDER").unwrap_or_default().to_lowercase();
        let explicit = if !env_explicit.is_empty() {
            env_explicit
        } else {
            json_provider.unwrap_or_default().to_lowercase()
        };
        if explicit == "anthropic" {
            return Self::Anthropic;
        }
        if explicit == "openai" {
            return Self::OpenAI;
        }
        // Auto-detect from model name or API base
        if model.starts_with("claude")
            || api_base.contains("anthropic.com")
        {
            Self::Anthropic
        } else {
            Self::OpenAI
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
        }
    }
}

pub(crate) struct Config {
    pub(crate) api_key: String,
    pub(crate) api_base: String,
    pub(crate) model: String,
    pub(crate) provider: Provider,
    pub(crate) providers: HashMap<String, JsonProviderConfig>,
    pub(crate) port: u16,
    pub(crate) max_context_tokens: usize,
    pub(crate) exec_timeout: Duration,
    pub(crate) max_tool_rounds: usize,
    pub(crate) max_output_bytes: usize,
    pub(crate) max_file_bytes: usize,
}

impl Config {
    pub(crate) fn load() -> Self {
        let json_cfg = load_config_file();
        let settings = json_cfg.settings.unwrap_or_default();
        let providers: HashMap<String, JsonProviderConfig> = json_cfg
            .models
            .and_then(|m| m.providers)
            .unwrap_or_default();

        // Default model: JSON agents.defaults.model.primary → env LINGCLAW_MODEL → "gpt-4o-mini"
        let default_from_json = json_cfg
            .agents
            .and_then(|a| a.defaults)
            .and_then(|d| d.model)
            .and_then(|m| m.primary);

        let model = default_from_json
            .or_else(|| std::env::var("LINGCLAW_MODEL").ok())
            .unwrap_or_else(|| "gpt-4o-mini".to_string());

        // API base: JSON settings.apiBase → env OPENAI_API_BASE → default
        let api_base = settings
            .api_base
            .clone()
            .or_else(|| std::env::var("OPENAI_API_BASE").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        let provider = Provider::detect(&model, &api_base, settings.provider.as_deref());

        // API key: JSON settings.apiKey → env vars → ""
        let api_key = settings.api_key.clone().unwrap_or_else(|| match provider {
            Provider::Anthropic => {
                std::env::var("ANTHROPIC_API_KEY")
                    .or_else(|_| std::env::var("OPENAI_API_KEY"))
                    .unwrap_or_default()
            }
            Provider::OpenAI => std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        });

        // Adjust api_base for Anthropic when still on default OpenAI URL
        let api_base = match provider {
            Provider::Anthropic => {
                if api_base == "https://api.openai.com/v1" {
                    "https://api.anthropic.com".to_string()
                } else {
                    api_base
                }
            }
            Provider::OpenAI => api_base,
        };

        Self {
            api_key,
            api_base,
            model,
            provider,
            providers,
            port: settings.port
                .or_else(|| std::env::var("LINGCLAW_PORT").ok()?.parse().ok())
                .unwrap_or(3000),
            max_context_tokens: settings.max_context_tokens
                .or_else(|| std::env::var("LINGCLAW_MAX_CONTEXT_TOKENS").ok()?.parse().ok())
                .unwrap_or(32000),
            exec_timeout: Duration::from_secs(
                settings.exec_timeout
                    .or_else(|| std::env::var("LINGCLAW_EXEC_TIMEOUT").ok()?.parse().ok())
                    .unwrap_or(30),
            ),
            max_tool_rounds: settings.max_tool_rounds
                .or_else(|| std::env::var("LINGCLAW_MAX_TOOL_ROUNDS").ok()?.parse().ok())
                .unwrap_or(20),
            max_output_bytes: settings.max_output_bytes.unwrap_or(50 * 1024),
            max_file_bytes: settings.max_file_bytes.unwrap_or(200 * 1024),
        }
    }

    /// Resolve a model reference ("provider/model" or plain "model-name") to
    /// a concrete provider, API base, API key, and model ID.
    fn resolve_model(&self, model_ref: &str) -> providers::ResolvedModel {
        // Try "provider/model" format
        if let Some((prov_name, model_id)) = model_ref.split_once('/') {
            if let Some(pc) = self.providers.get(prov_name) {
                return providers::ResolvedModel {
                    provider: match pc.api.as_str() {
                        "anthropic" => Provider::Anthropic,
                        _ => Provider::OpenAI,
                    },
                    api_base: pc.base_url.clone(),
                    api_key: pc.api_key.clone(),
                    model_id: model_id.to_string(),
                };
            }
        }
        // Fallback to env-based config
        providers::ResolvedModel {
            provider: self.provider,
            api_base: self.api_base.clone(),
            api_key: self.api_key.clone(),
            model_id: model_ref.to_string(),
        }
    }

    /// List all available models: from config file providers + the default env model.
    fn available_models(&self) -> Vec<String> {
        let mut models: Vec<String> = Vec::new();
        for (prov_name, pc) in &self.providers {
            for m in &pc.models {
                models.push(format!("{prov_name}/{}", m.id));
            }
        }
        if models.is_empty() || !models.iter().any(|m| m == &self.model) {
            models.push(self.model.clone());
        }
        models
    }
}

// ── Config File (lingclaw.json) ──────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct JsonConfig {
    settings: Option<JsonSettings>,
    models: Option<JsonModelsConfig>,
    agents: Option<JsonAgentsConfig>,
}

#[derive(Deserialize, Default)]
struct JsonSettings {
    port: Option<u16>,
    provider: Option<String>,
    #[serde(rename = "apiKey")]
    api_key: Option<String>,
    #[serde(rename = "apiBase")]
    api_base: Option<String>,
    #[serde(rename = "execTimeout")]
    exec_timeout: Option<u64>,
    #[serde(rename = "maxToolRounds")]
    max_tool_rounds: Option<usize>,
    #[serde(rename = "maxContextTokens")]
    max_context_tokens: Option<usize>,
    #[serde(rename = "maxOutputBytes")]
    max_output_bytes: Option<usize>,
    #[serde(rename = "maxFileBytes")]
    max_file_bytes: Option<usize>,
}

#[derive(Deserialize, Default)]
struct JsonModelsConfig {
    providers: Option<HashMap<String, JsonProviderConfig>>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct JsonProviderConfig {
    #[serde(rename = "baseUrl")]
    pub(crate) base_url: String,
    #[serde(rename = "apiKey")]
    pub(crate) api_key: String,
    #[serde(default = "default_api_protocol")]
    pub(crate) api: String,
    #[serde(default)]
    pub(crate) models: Vec<JsonModelEntry>,
}

fn default_api_protocol() -> String {
    "openai-completions".to_string()
}

#[derive(Deserialize, Serialize, Clone, Default)]
pub(crate) struct JsonModelEntry {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cost: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "contextWindow")]
    pub(crate) context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxTokens")]
    pub(crate) max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) compat: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct JsonAgentsConfig {
    defaults: Option<JsonAgentDefaults>,
}

#[derive(Deserialize, Default)]
struct JsonAgentDefaults {
    model: Option<JsonDefaultModel>,
}

#[derive(Deserialize, Default)]
struct JsonDefaultModel {
    primary: Option<String>,
}

pub(crate) fn config_dir_path() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    if home.is_empty() {
        return None;
    }
    Some(Path::new(&home).join(".lingclaw"))
}

pub(crate) fn config_file_path() -> Option<PathBuf> {
    Some(config_dir_path()?.join(".lingclaw.json"))
}

fn load_config_file() -> JsonConfig {
    let path = match config_file_path() {
        Some(p) => p,
        None => return JsonConfig::default(),
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            eprintln!("WARNING: Failed to parse {}: {e}", path.display());
            JsonConfig::default()
        }),
        Err(_) => JsonConfig::default(),
    }
}


// ══════════════════════════════════════════════════════════════════════════════
//  Data Models
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Serialize, Deserialize, Debug)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: FunctionCall,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct FunctionCall {
    name: String,
    arguments: String,
}

// ══════════════════════════════════════════════════════════════════════════════
//  Session & AppState
// ══════════════════════════════════════════════════════════════════════════════

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn gen_session_id() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}{:04x}", t.as_secs(), t.subsec_nanos() % 0xFFFF)
}

#[derive(Clone, Serialize, Deserialize)]
struct Session {
    id: String,
    name: String,
    messages: Vec<ChatMessage>,
    created_at: u64,
    updated_at: u64,
    tool_calls_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_override: Option<String>,
    #[serde(default = "default_think_level")]
    think_level: String,
    #[serde(skip)]
    workspace: PathBuf,
}

fn default_think_level() -> String {
    "off".to_string()
}

/// Per-session workspace: ~/.lingclaw/{sessionId}/workspace
fn session_workspace_path(session_id: &str) -> PathBuf {
    config_dir_path()
        .unwrap_or_else(|| PathBuf::from(".lingclaw"))
        .join(session_id)
        .join("workspace")
}

impl Session {
    fn new() -> Self {
        let id = gen_session_id();
        let workspace = session_workspace_path(&id);
        std::fs::create_dir_all(&workspace).ok();
        prompts::init_session_prompt_files(&workspace);
        Self {
            id,
            name: "New Chat".into(),
            messages: Vec::new(),
            created_at: now_epoch(),
            updated_at: now_epoch(),
            tool_calls_count: 0,
            model_override: None,
            think_level: "off".into(),
            workspace,
        }
    }

    fn effective_model<'a>(&'a self, default: &'a str) -> &'a str {
        self.model_override.as_deref().unwrap_or(default)
    }
}

struct AppState {
    config: Config,
    http: Client,
    sessions: Mutex<HashMap<String, Session>>,
    shutdown: CancellationToken,
    shutdown_token: String,
}

// ══════════════════════════════════════════════════════════════════════════════
//  System Prompt
// ══════════════════════════════════════════════════════════════════════════════

fn build_system_prompt(config: &Config, workspace: &Path, model: &str) -> ChatMessage {
    let os_name = if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    let cwd = workspace.display();
    let tool_lines = tools::render_tool_prompt_lines(config);
    let persona = prompts::load_session_prompt_files(workspace);

    let prompt = format!(
        r#"{persona}

---

## Environment
- OS: {os_name}
- Working directory: {cwd}
- Model: {model}

## Available Tools
{tool_lines}"#,
        model = model,
        tool_lines = tool_lines,
        persona = persona,
    );

    ChatMessage {
        role: "system".into(),
        content: Some(prompt),
        tool_calls: None,
        tool_call_id: None,
    }
}

// ══════════════════════════════════════════════════════════════════════════════
//  Security
// ══════════════════════════════════════════════════════════════════════════════

const DANGEROUS_PATTERNS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "mkfs.",
    "dd if=/dev",
    ":(){ :|:&",
    "> /dev/sda",
    "format c:",
    "del /f /s /q c:\\",
    "rd /s /q c:\\",
];

fn check_dangerous_command(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_lowercase();
    DANGEROUS_PATTERNS
        .iter()
        .find(|&&pattern| lower.contains(pattern))
        .copied()
}

fn resolve_path(path_str: &str, workspace: &Path) -> PathBuf {
    let p = Path::new(path_str);
    let candidate = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace.join(p)
    };
    // Normalize the path (resolve .., symlinks) and verify it stays inside workspace.
    // If canonicalize fails (file doesn't exist yet), manually strip `..` components.
    let resolved = candidate.canonicalize().unwrap_or_else(|_| {
        let mut parts = Vec::new();
        for comp in candidate.components() {
            match comp {
                std::path::Component::ParentDir => { parts.pop(); }
                std::path::Component::CurDir => {}
                c => parts.push(c.as_os_str().to_owned()),
            }
        }
        parts.iter().collect()
    });
    let ws_canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    if resolved.starts_with(&ws_canonical) {
        resolved
    } else {
        // Path escapes workspace — clamp to workspace root
        eprintln!("SECURITY: path '{}' escapes workspace, clamped", path_str);
        ws_canonical
    }
}

// ══════════════════════════════════════════════════════════════════════════════
//  Utilities
// ══════════════════════════════════════════════════════════════════════════════

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!(
            "{}...\n[truncated at {} bytes, total {} bytes]",
            &s[..max],
            max,
            s.len()
        )
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn matches_glob(name: &str, pattern: &str) -> bool {
    if let Some(ext) = pattern.strip_prefix("*.") {
        name.ends_with(&format!(".{ext}"))
    } else if let Some(prefix) = pattern.strip_suffix('*') {
        name.starts_with(prefix)
    } else {
        name == pattern
    }
}

type WsTx = SplitSink<WebSocket, WsMsg>;

async fn ws_send(tx: &mut WsTx, data: &serde_json::Value) {
    let _ = tx.send(WsMsg::Text(data.to_string().into())).await;
}

// ══════════════════════════════════════════════════════════════════════════════
//  Tool Dispatch
// ══════════════════════════════════════════════════════════════════════════════

async fn execute_tool(name: &str, args_str: &str, config: &Config, http: &Client, workspace: &Path) -> String {
    tools::execute_tool(name, args_str, config, http, workspace).await
}

// ══════════════════════════════════════════════════════════════════════════════
//  Context Management
// ══════════════════════════════════════════════════════════════════════════════

fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content_len = m.content.as_ref().map(|c| c.len()).unwrap_or(0);
            let tc_len = m
                .tool_calls
                .as_ref()
                .map(|tcs| {
                    tcs.iter()
                        .map(|tc| tc.function.name.len() + tc.function.arguments.len())
                        .sum::<usize>()
                })
                .unwrap_or(0);
            (content_len + tc_len + 10) / 4 // rough ~4 chars per token
        })
        .sum()
}

fn prune_messages(messages: &mut Vec<ChatMessage>, max_tokens: usize) {
    // Keep: system message (index 0) + as many recent messages as fit.
    // Remove oldest non-system messages when over budget.
    while estimate_tokens(messages) > max_tokens && messages.len() > 2 {
        messages.remove(1);
    }
}

// ══════════════════════════════════════════════════════════════════════════════
//  Session Persistence
// ══════════════════════════════════════════════════════════════════════════════

fn sessions_dir() -> PathBuf {
    let dir = config_dir_path()
        .unwrap_or_else(|| PathBuf::from(".lingclaw"))
        .join("sessions");
    std::fs::create_dir_all(&dir).ok();
    dir
}

async fn save_session_to_disk(session: &Session) -> Result<(), String> {
    let path = sessions_dir().join(format!("{}.json", session.id));
    let data = serde_json::to_string_pretty(session).map_err(|e| e.to_string())?;
    tokio::fs::write(&path, data)
        .await
        .map_err(|e| e.to_string())
}

/// Remove trailing incomplete tool-call transactions from session messages.
/// If the last assistant message has tool_calls but not all matching tool results
/// follow it, pop the assistant message and any partial tool results.
/// This prevents persisting an invalid message sequence on shutdown interruption.
fn trim_incomplete_tool_calls(messages: &mut Vec<ChatMessage>) {
    // Find last assistant message with tool_calls
    let ast_idx = messages.iter().rposition(|m| {
        m.role == "assistant" && m.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
    });
    let Some(idx) = ast_idx else { return };
    let expected = messages[idx]
        .tool_calls
        .as_ref()
        .map(|tc| tc.len())
        .unwrap_or(0);
    // Count tool messages that follow the assistant message
    let actual = messages[idx + 1..]
        .iter()
        .filter(|m| m.role == "tool")
        .count();
    if actual < expected {
        // Incomplete: remove assistant + any partial tool results
        messages.truncate(idx);
    }
}

fn load_session_from_disk(id: &str) -> Option<Session> {
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return None;
    }
    let path = sessions_dir().join(format!("{id}.json"));
    let data = std::fs::read_to_string(&path).ok()?;
    let mut session: Session = serde_json::from_str(&data).ok()?;
    session.workspace = session_workspace_path(&session.id);
    std::fs::create_dir_all(&session.workspace).ok();
    prompts::init_session_prompt_files(&session.workspace);
    Some(session)
}

/// List all saved session summaries from disk, sorted by created_at desc.
fn list_saved_session_summaries() -> Vec<serde_json::Value> {
    let dir = sessions_dir();
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(data) = std::fs::read_to_string(&path) {
                    if let Ok(s) = serde_json::from_str::<Session>(&data) {
                        let msg_count = s.messages.iter().filter(|m| m.role != "system").count();
                        out.push(json!({
                            "id": s.id,
                            "name": s.name,
                            "messages": msg_count,
                            "created_at": s.created_at,
                            "updated_at": s.updated_at,
                        }));
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| {
        let b_ts = b["created_at"].as_u64().unwrap_or(0);
        let a_ts = a["created_at"].as_u64().unwrap_or(0);
        b_ts.cmp(&a_ts)
    });
    out
}

fn build_history_payload(session: &Session) -> serde_json::Value {
    let mut msgs = Vec::new();
    for msg in &session.messages {
        match msg.role.as_str() {
            "system" => {}
            "user" => {
                if let Some(c) = &msg.content {
                    msgs.push(json!({"role":"user","content":c}));
                }
            }
            "assistant" => {
                if let Some(c) = &msg.content {
                    if !c.is_empty() {
                        msgs.push(json!({"role":"assistant","content":c}));
                    }
                }
                if let Some(tcs) = &msg.tool_calls {
                    for tc in tcs {
                        msgs.push(json!({"role":"tool_call","name":tc.function.name,"arguments":tc.function.arguments,"id":tc.id}));
                    }
                }
            }
            "tool" => {
                if let Some(c) = &msg.content {
                    msgs.push(json!({"role":"tool_result","result":c,"id":msg.tool_call_id.as_deref().unwrap_or("")}));
                }
            }
            _ => {}
        }
    }
    json!({"type":"history","messages":msgs})
}

// ══════════════════════════════════════════════════════════════════════════════
//  Chat Commands
// ══════════════════════════════════════════════════════════════════════════════

struct CommandResult {
    response: String,
    new_session_id: Option<String>,
    sessions_changed: bool,
}

async fn handle_command(
    input: &str,
    current_session_id: &str,
    state: &AppState,
    cancel: &CancellationToken,
) -> Option<CommandResult> {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd {
        "/new" => {
            // Compress conversation → save to memory → clear context
            let (conversation_text, workspace, model_str) = {
                let sessions = state.sessions.lock().await;
                let session = match sessions.get(current_session_id) {
                    Some(s) => s,
                    None => return Some(CommandResult {
                        response: "Session not found".into(),
                        new_session_id: None,
                        sessions_changed: false,
                    }),
                };
                // Build plain-text conversation (skip system prompt)
                let mut lines = Vec::new();
                for msg in &session.messages {
                    match msg.role.as_str() {
                        "user" => {
                            if let Some(c) = &msg.content {
                                lines.push(format!("User: {c}"));
                            }
                        }
                        "assistant" => {
                            if let Some(c) = &msg.content {
                                if !c.is_empty() {
                                    lines.push(format!("Assistant: {c}"));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                (lines.join("\n"), session.workspace.clone(), session.effective_model(&state.config.model).to_string())
            };

            if conversation_text.is_empty() {
                // Nothing to compress — just clear
                let mut sessions = state.sessions.lock().await;
                if let Some(session) = sessions.get_mut(current_session_id) {
                    let model = session.effective_model(&state.config.model).to_string();
                    let sys = build_system_prompt(&state.config, &session.workspace, &model);
                    session.messages = vec![sys];
                    session.tool_calls_count = 0;
                    session.updated_at = now_epoch();
                }
                return Some(CommandResult {
                    response: "Context cleared.".into(),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }

            // Ask LLM to compress
            let compress_prompt = vec![
                ChatMessage {
                    role: "system".into(),
                    content: Some("You are a conversation summarizer. Compress the following conversation into a concise markdown summary. Keep key decisions, code changes, problems solved, and important context. Use bullet points. Write in the same language as the conversation. Do NOT wrap in code blocks.".into()),
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".into(),
                    content: Some(conversation_text),
                    tool_calls: None,
                    tool_call_id: None,
                },
            ];
            let resolved = state.config.resolve_model(&model_str);
            let summary = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return Some(CommandResult {
                        response: "Shutdown: compression skipped, context unchanged.".into(),
                        new_session_id: None,
                        sessions_changed: false,
                    });
                }
                result = providers::call_llm_simple(&state.http, &resolved, &compress_prompt) => {
                    match result {
                        Ok(s) => s,
                        Err(e) => {
                            return Some(CommandResult {
                                response: format!("Failed to compress conversation: {e}"),
                                new_session_id: None,
                                sessions_changed: false,
                            });
                        }
                    }
                }
            };

            // Append to memory/YYYY-MM-DD.md
            let today = prompts::chrono_today();
            let memory_dir = workspace.join("memory");
            std::fs::create_dir_all(&memory_dir).ok();
            let memory_path = memory_dir.join(format!("{today}.md"));

            let entry = {
                let secs = now_epoch();
                let hh = (secs % 86400) / 3600;
                let mm = (secs % 3600) / 60;
                format!("\n\n---\n\n## {hh:02}:{mm:02}\n\n{summary}", summary = summary.trim())
            };

            // Use a block for the file I/O so it doesn't conflict with async
            let write_result = if memory_path.exists() {
                // Append
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&memory_path)
                    .and_then(|mut f| f.write_all(entry.as_bytes()))
            } else {
                // Create with header
                let content = format!("# {today}\n{entry}");
                std::fs::write(&memory_path, content)
            };

            if let Err(e) = write_result {
                return Some(CommandResult {
                    response: format!("Failed to write memory: {e}"),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }

            // Clear context
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                let model = session.effective_model(&state.config.model).to_string();
                let sys = build_system_prompt(&state.config, &session.workspace, &model);
                session.messages = vec![sys];
                session.tool_calls_count = 0;
                session.updated_at = now_epoch();
            }

            Some(CommandResult {
                response: format!("Conversation compressed and saved to memory/{today}.md. Context cleared."),
                new_session_id: None,
                sessions_changed: false,
            })
        }

        "/session_new" => {
            // Remove current session from memory (release lock before disk I/O)
            let old = {
                let mut sessions = state.sessions.lock().await;
                sessions.remove(current_session_id)
            };
            if let Some(ref old) = old {
                if old.messages.len() > 1 {
                    let _ = save_session_to_disk(old).await;
                }
            }
            // Create brand-new session
            let mut s = Session::new();
            let model = s.effective_model(&state.config.model).to_string();
            let sys = build_system_prompt(&state.config, &s.workspace, &model);
            s.messages.push(sys);
            let new_id = s.id.clone();
            state.sessions.lock().await.insert(new_id.clone(), s);
            Some(CommandResult {
                response: "A new journey begins.".into(),
                new_session_id: Some(new_id),
                sessions_changed: true,
            })
        }

        "/switch" => {
            if arg.is_empty() {
                return Some(CommandResult {
                    response: "Usage: /switch <session_id>".into(),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let target = arg.to_string();
            if target == current_session_id {
                return Some(CommandResult {
                    response: "Already on this session.".into(),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            // Prevent switching to a session already owned by another connection.
            // In-memory sessions are exclusively owned; only disk sessions are available.
            {
                let sessions = state.sessions.lock().await;
                if sessions.contains_key(&target) {
                    return Some(CommandResult {
                        response: format!("Session '{}' is in use by another connection.", &target[..12.min(target.len())]),
                        new_session_id: None,
                        sessions_changed: false,
                    });
                }
            }
            // Try loading from disk
            let disk_session = load_session_from_disk(&target);
            if disk_session.is_none() {
                return Some(CommandResult {
                    response: format!("Session '{}' not found.", &target[..12.min(target.len())]),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            // Save current session outside the lock (avoid blocking other connections)
            let old = {
                let mut sessions = state.sessions.lock().await;
                sessions.remove(current_session_id)
            };
            if let Some(ref old) = old {
                if old.messages.len() > 1 {
                    let _ = save_session_to_disk(old).await;
                }
            }
            // Atomically claim target — double-check after disk I/O
            let mut sessions = state.sessions.lock().await;
            if sessions.contains_key(&target) {
                // Another connection claimed it — re-insert our old session
                if let Some(old) = old {
                    sessions.insert(old.id.clone(), old);
                }
                return Some(CommandResult {
                    response: format!("Session '{}' is in use by another connection.", &target[..12.min(target.len())]),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let mut s = disk_session.unwrap();
            let model = s.effective_model(&state.config.model).to_string();
            let sys = build_system_prompt(&state.config, &s.workspace, &model);
            if let Some(first) = s.messages.first_mut() {
                if first.role == "system" {
                    *first = sys;
                }
            }
            let id = s.id.clone();
            sessions.insert(id.clone(), s);
            Some(CommandResult {
                response: format!("Loaded session {}", &id[..12.min(id.len())]),
                new_session_id: Some(id),
                sessions_changed: true,
            })
        }

        "/rename" => {
            if arg.is_empty() {
                return Some(CommandResult {
                    response: "Usage: /rename <new_name>".into(),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                session.name = arg.to_string();
                Some(CommandResult {
                    response: format!("Renamed to: {arg}"),
                    new_session_id: None,
                    sessions_changed: true,
                })
            } else {
                Some(CommandResult {
                    response: "Current session not found".into(),
                    new_session_id: None,
                    sessions_changed: false,
                })
            }
        }

        "/model" => {
            if arg.is_empty() {
                let sessions = state.sessions.lock().await;
                let model = sessions
                    .get(current_session_id)
                    .map(|s| s.effective_model(&state.config.model))
                    .unwrap_or(&state.config.model);
                let available = state.config.available_models();
                let list = available
                    .iter()
                    .map(|m| {
                        if m == model {
                            format!("  * {m} (current)")
                        } else {
                            format!("    {m}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(CommandResult {
                    response: format!("Available models:\n{list}\n\nUse /model <name> to switch."),
                    new_session_id: None,
                    sessions_changed: false,
                })
            } else {
                let mut sessions = state.sessions.lock().await;
                if let Some(session) = sessions.get_mut(current_session_id) {
                    session.model_override = Some(arg.to_string());
                    Some(CommandResult {
                        response: format!("Model switched to: {arg}"),
                        new_session_id: None,
                        sessions_changed: true,
                    })
                } else {
                    Some(CommandResult {
                        response: "Session not found".into(),
                        new_session_id: None,
                        sessions_changed: false,
                    })
                }
            }
        }

        "/status" => {
            let sessions = state.sessions.lock().await;
            match sessions.get(current_session_id) {
                Some(s) => {
                    let model_ref = s.effective_model(&state.config.model);
                    let tokens = estimate_tokens(&s.messages);
                    Some(CommandResult {
                        response: format!(
                            "agent: LingClaw\n\
                             model: {model_ref}\n\
                             context: {tokens}/{max_ctx}\n\
                             think: {think}",
                            max_ctx = state.config.max_context_tokens,
                            think = s.think_level,
                        ),
                        new_session_id: None,
                        sessions_changed: false,
                    })
                }
                None => Some(CommandResult {
                    response: "No active session".into(),
                    new_session_id: None,
                    sessions_changed: false,
                }),
            }
        }

        "/clear" => {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                let model = session.effective_model(&state.config.model).to_string();
                let system_msg = build_system_prompt(&state.config, &session.workspace, &model);
                session.messages = vec![system_msg];
                session.tool_calls_count = 0;
                session.updated_at = now_epoch();
                Some(CommandResult {
                    response: "Session cleared. System prompt preserved.".into(),
                    new_session_id: None,
                    sessions_changed: false,
                })
            } else {
                Some(CommandResult {
                    response: "Session not found".into(),
                    new_session_id: None,
                    sessions_changed: false,
                })
            }
        }

        "/skills" => {
            let list = tools::tool_specs()
                .iter()
                .map(|spec| {
                    let short = spec.description.split('.').next().unwrap_or(spec.description);
                    format!("  {} → {}", spec.name, short)
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(CommandResult {
                response: format!("Skills:\n{list}"),
                new_session_id: None,
                sessions_changed: false,
            })
        }

        "/think" => {
            const VALID_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh"];
            if arg.is_empty() {
                let sessions = state.sessions.lock().await;
                let level = sessions
                    .get(current_session_id)
                    .map(|s| s.think_level.as_str())
                    .unwrap_or("off");
                return Some(CommandResult {
                    response: format!("think: {level}\nUsage: /think <off|minimal|low|medium|high|xhigh>"),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let level = arg.to_lowercase();
            if !VALID_LEVELS.contains(&level.as_str()) {
                return Some(CommandResult {
                    response: format!("Invalid think level: {arg}\nValid: off, minimal, low, medium, high, xhigh"),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                session.think_level = level.clone();
                Some(CommandResult {
                    response: format!("Think mode set to: {level}"),
                    new_session_id: None,
                    sessions_changed: true,
                })
            } else {
                Some(CommandResult {
                    response: "Session not found".into(),
                    new_session_id: None,
                    sessions_changed: false,
                })
            }
        }

        "/help" => Some(CommandResult {
            response: "\
Commands:
  /new             Compress conversation to memory & clear context
  /status          Show session status
  /model [name]    Show or switch model
  /think [level]   Set thinking mode (off|minimal|low|medium|high|xhigh)
  /skills          List available skills
  /rename <name>   Rename current session
  /clear           Clear messages (keep system prompt)
  /help            Show this help"
                .into(),
            new_session_id: None,
            sessions_changed: false,
        }),

        _ => None,
    }
}

// ══════════════════════════════════════════════════════════════════════════════
//  WebSocket Handler
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Deserialize)]
struct WsQuery {
    session: Option<String>,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(query): Query<WsQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let requested = query.session.filter(|s| !s.is_empty());
    ws.on_upgrade(|socket| handle_socket(socket, state, requested))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>, requested_id: Option<String>) {
    let (mut tx, mut rx) = socket.split();

    // Resume a session: prefer the explicitly requested one (browser refresh),
    // then fall back to the most recent unclaimed saved session, then create new.
    let mut current_session_id;

    let mut claimed = if let Some(ref req_id) = requested_id {
        // Client requested a specific session — wait briefly for the old connection to release it
        claim_requested_session(req_id, &state).await
    } else {
        None
    };

    // If no explicit request or requested session unavailable, try saved sessions in recency order
    if claimed.is_none() {
        let saved_ids: Vec<String> = list_saved_session_summaries()
            .iter()
            .filter_map(|s| s["id"].as_str().map(|id| id.to_string()))
            .collect();

        for cid in &saved_ids {
            if let Some(mut session) = load_session_from_disk(cid) {
                let mut sessions = state.sessions.lock().await;
                if sessions.contains_key(&session.id) {
                    continue;
                }
                let model = session.effective_model(&state.config.model).to_string();
                let sys = build_system_prompt(&state.config, &session.workspace, &model);
                if let Some(first) = session.messages.first_mut() {
                    if first.role == "system" {
                        *first = sys;
                    }
                }
                let id = session.id.clone();
                sessions.insert(id.clone(), session);
                claimed = Some(id);
                break;
            }
        }
    }

    if let Some(id) = claimed {
        current_session_id = id.clone();
        let (name, history) = {
            let sessions = state.sessions.lock().await;
            let s = sessions.get(&id).expect("just claimed");
            (s.name.clone(), build_history_payload(s))
        };
        ws_send(&mut tx, &json!({"type":"session","id":&id,"name":&name})).await;
        ws_send(&mut tx, &history).await;
    } else {
        let mut session = Session::new();
        let model = session.effective_model(&state.config.model).to_string();
        let sys = build_system_prompt(&state.config, &session.workspace, &model);
        session.messages.push(sys);
        current_session_id = session.id.clone();
        ws_send(&mut tx, &json!({"type":"session","id":&current_session_id,"name":"New Chat"})).await;
        state.sessions.lock().await.insert(current_session_id.clone(), session);
    }

    send_sessions_list(&mut tx, &state, &current_session_id).await;

    let cancel = state.shutdown.clone();
    loop {
        let msg = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            result = rx.next() => match result {
                Some(Ok(m)) => m,
                _ => break,
            },
        };
        let text = match msg {
            WsMsg::Text(t) => t.to_string(),
            WsMsg::Close(_) => break,
            _ => continue,
        };

        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        // ── Slash commands ──────────────────────────────────────────────
        if trimmed.starts_with('/') {
            let cmd_result = handle_command(trimmed, &current_session_id, &state, &cancel).await;
            if cancel.is_cancelled() {
                break;
            }
            if let Some(result) = cmd_result {
                ws_send(
                    &mut tx,
                    &json!({"type":"system","content":result.response}),
                )
                .await;

                if let Some(new_id) = result.new_session_id {
                    current_session_id = new_id.clone();
                    let name = {
                        let sessions = state.sessions.lock().await;
                        sessions.get(&current_session_id).map(|s| s.name.clone())
                            .unwrap_or_else(|| "New Chat".into())
                    };
                    ws_send(&mut tx, &json!({"type":"session_switched","id":&new_id,"name":&name})).await;
                    // Send chat history for the switched-to session
                    let history = {
                        let sessions = state.sessions.lock().await;
                        sessions.get(&current_session_id).map(build_history_payload)
                    };
                    if let Some(payload) = history {
                        ws_send(&mut tx, &payload).await;
                    }
                }
                if result.sessions_changed {
                    send_sessions_list(&mut tx, &state, &current_session_id).await;
                }
            } else {
                ws_send(
                    &mut tx,
                    &json!({"type":"system","content":"Unknown command. Type /help."}),
                )
                .await;
            }
            continue;
        }

        // ── Normal message → Agent loop ─────────────────────────────────
        {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(&current_session_id) {
                session.messages.push(ChatMessage {
                    role: "user".into(),
                    content: Some(text),
                    tool_calls: None,
                    tool_call_id: None,
                });
                session.updated_at = now_epoch();
            }
        }

        let mut completed = false;
        let mut shutting_down = false;
        'agent: for round in 0..state.config.max_tool_rounds {
            // Check shutdown before starting a new round
            if cancel.is_cancelled() {
                shutting_down = true;
                break;
            }

            // Snapshot messages (prune if needed)
            let (msgs_snapshot, model, workspace) = {
                let mut sessions = state.sessions.lock().await;
                let session = match sessions.get_mut(&current_session_id) {
                    Some(s) => s,
                    None => break,
                };
                // Refresh system prompt so prompt-file edits take effect mid-session
                let model_str = session.effective_model(&state.config.model).to_string();
                let fresh_system = build_system_prompt(&state.config, &session.workspace, &model_str);
                if let Some(first) = session.messages.first_mut() {
                    if first.role == "system" {
                        *first = fresh_system;
                    }
                }
                prune_messages(&mut session.messages, state.config.max_context_tokens);
                (
                    session.messages.clone(),
                    model_str,
                    session.workspace.clone(),
                )
            };

            ws_send(&mut tx, &json!({"type":"start","round":round + 1})).await;

            let resolved = state.config.resolve_model(&model);

            // Wrap LLM call in select so shutdown can interrupt a long stream
            let llm_result = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    shutting_down = true;
                    break;
                }
                r = providers::call_llm_stream(
                    &state.http,
                    &resolved,
                    &msgs_snapshot,
                    &mut tx,
                ) => r,
            };
            match llm_result {
                Ok(resp) => {
                    let has_tools = resp.message.tool_calls.is_some();

                    // Push assistant message to session
                    {
                        let mut sessions = state.sessions.lock().await;
                        if let Some(session) = sessions.get_mut(&current_session_id) {
                            session.messages.push(resp.message.clone());
                            session.updated_at = now_epoch();
                        }
                    }

                    if let Some(tool_calls) = &resp.message.tool_calls {
                        for tc in tool_calls {
                            // Check shutdown before each tool call
                            if cancel.is_cancelled() {
                                shutting_down = true;
                                break 'agent;
                            }

                            ws_send(
                                &mut tx,
                                &json!({
                                    "type":"tool_call",
                                    "id": tc.id,
                                    "name": tc.function.name,
                                    "arguments": tc.function.arguments,
                                }),
                            )
                            .await;

                            let result = tokio::select! {
                                biased;
                                _ = cancel.cancelled() => {
                                    shutting_down = true;
                                    break 'agent;
                                }
                                r = execute_tool(
                                    &tc.function.name,
                                    &tc.function.arguments,
                                    &state.config,
                                    &state.http,
                                    &workspace,
                                ) => r,
                            };

                            ws_send(
                                &mut tx,
                                &json!({
                                    "type":"tool_result",
                                    "id": tc.id,
                                    "name": tc.function.name,
                                    "result": result,
                                }),
                            )
                            .await;

                            // Push tool result to session
                            let mut sessions = state.sessions.lock().await;
                            if let Some(session) = sessions.get_mut(&current_session_id) {
                                session.messages.push(ChatMessage {
                                    role: "tool".into(),
                                    content: Some(result),
                                    tool_calls: None,
                                    tool_call_id: Some(tc.id.clone()),
                                });
                                session.tool_calls_count += 1;
                            }
                        }
                        // Loop continues → LLM processes tool results
                    }

                    if !has_tools {
                        ws_send(&mut tx, &json!({"type":"done"})).await;
                        completed = true;
                        break;
                    }
                }
                Err(e) => {
                    ws_send(&mut tx, &json!({"type":"error","content":e})).await;
                    completed = true;
                    break;
                }
            }
        }

        if shutting_down {
            ws_send(
                &mut tx,
                &json!({"type":"system","content":"Server shutting down."}),
            )
            .await;
        } else if !completed {
            ws_send(
                &mut tx,
                &json!({
                    "type":"system",
                    "content": format!(
                        "Reached maximum tool rounds ({}). Stopping.",
                        state.config.max_tool_rounds
                    )
                }),
            )
            .await;
        }

        // Auto-save session to disk after each completed exchange
        // If shutting down, trim incomplete tool-call transactions first
        if shutting_down {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(&current_session_id) {
                trim_incomplete_tool_calls(&mut session.messages);
            }
        }
        let snapshot = {
            let sessions = state.sessions.lock().await;
            sessions.get(&current_session_id).cloned()
        };
        if let Some(ref s) = snapshot {
            if s.messages.len() > 1 {
                let _ = save_session_to_disk(s).await;
            }
        }

        // If shutting down, break out of the main message loop
        if shutting_down {
            break;
        }
    }

    // ── Cleanup: remove from memory first (release lock), then save to disk ────
    {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(&current_session_id) {
            trim_incomplete_tool_calls(&mut session.messages);
        }
    }
    let disconnected_session = {
        let mut sessions = state.sessions.lock().await;
        sessions.remove(&current_session_id)
    };
    if let Some(ref session) = disconnected_session {
        if session.messages.len() > 1 {
            let _ = save_session_to_disk(session).await;
        } else {
            // Empty session — clean up its workspace directory
            let _ = std::fs::remove_dir_all(&session.workspace);
        }
    }
}

/// Wait for a specific session to become available (old connection releasing it),
/// then load from disk and claim it. Returns None if unavailable after timeout.
async fn claim_requested_session(id: &str, state: &AppState) -> Option<String> {
    // Validate ID format
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return None;
    }
    // Wait up to 3 seconds for the old connection to release the session
    for _ in 0..6 {
        let in_use = state.sessions.lock().await.contains_key(id);
        if !in_use {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    // Try to load and claim
    let mut session = load_session_from_disk(id)?;
    let mut sessions = state.sessions.lock().await;
    if sessions.contains_key(id) {
        return None; // Still occupied after timeout
    }
    let model = session.effective_model(&state.config.model).to_string();
    let sys = build_system_prompt(&state.config, &session.workspace, &model);
    if let Some(first) = session.messages.first_mut() {
        if first.role == "system" {
            *first = sys;
        }
    }
    let sid = session.id.clone();
    sessions.insert(sid.clone(), session);
    Some(sid)
}

async fn send_sessions_list(tx: &mut WsTx, state: &AppState, active_id: &str) {
    // Merge in-memory sessions with on-disk summaries
    let in_mem: HashMap<String, serde_json::Value> = {
        let sessions = state.sessions.lock().await;
        sessions.iter().map(|(id, s)| {
            let msg_count = s.messages.iter().filter(|m| m.role != "system").count();
            (id.clone(), json!({
                "id": id, "name": s.name, "messages": msg_count,
                "created_at": s.created_at, "active": id == active_id,
            }))
        }).collect()
    };
    let mut all = list_saved_session_summaries();
    for item in &mut all {
        let id = item["id"].as_str().unwrap_or_default().to_string();
        if let Some(mem) = in_mem.get(&id) {
            *item = mem.clone();
        } else {
            item["active"] = json!(id == active_id);
        }
    }
    for (id, val) in &in_mem {
        if !all.iter().any(|s| s["id"].as_str() == Some(id)) {
            all.push(val.clone());
        }
    }
    all.sort_by(|a, b| {
        let b_ts = b["created_at"].as_u64().unwrap_or(0);
        let a_ts = a["created_at"].as_u64().unwrap_or(0);
        b_ts.cmp(&a_ts)
    });
    ws_send(tx, &json!({"type":"sessions_list","sessions":all})).await;
}

// ══════════════════════════════════════════════════════════════════════════════
//  HTTP API
// ══════════════════════════════════════════════════════════════════════════════

async fn api_shutdown(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    // Verify shutdown token — only the local CLI should be able to trigger this
    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if provided != state.shutdown_token {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "unauthorized" })));
    }

    // Signal shutdown — each WebSocket handler saves its own session on exit,
    // and main() does a final flush of any remaining sessions.
    state.shutdown.cancel();
    (StatusCode::OK, Json(json!({ "status": "shutting_down" })))
}

async fn api_health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sessions = state.sessions.lock().await;
    Json(json!({
        "status": "ok",
        "model": state.config.model,
        "sessions": sessions.len(),
    }))
}

async fn api_sessions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sessions = state.sessions.lock().await;
    let list: Vec<serde_json::Value> = sessions
        .iter()
        .map(|(id, s)| {
            json!({
                "id": id,
                "name": s.name,
                "messages": s.messages.len(),
                "tool_calls": s.tool_calls_count,
                "model": s.effective_model(&state.config.model),
                "created_at": s.created_at,
                "updated_at": s.updated_at,
            })
        })
        .collect();
    Json(json!({"sessions": list}))
}

// ══════════════════════════════════════════════════════════════════════════════
//  Main
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Parse --port <N> from anywhere in args
    let port_override: Option<u16> = args.windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok());

    // --version / -V: print and exit early
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("lingclaw v{VERSION}");
        return;
    }

    // CLI subcommands: lingclaw start|stop|restart|health|status|update
    if args.len() > 1
        && !args[1].starts_with('-')
        && cli::handle_cli_command(&args[1], port_override)
    {
        return;
    }

    let force_wizard = args.iter().any(|a| a == "--install-daemon");
    let serve_mode = args.iter().any(|a| a == "--serve");

    // First-run setup wizard (before loading config)
    if !cli::run_setup_wizard(force_wizard) {
        return;
    }

    // Default behavior (no --serve): start as daemon
    if !serve_mode {
        cli::handle_cli_command("start", port_override);
        return;
    }

    let config = Config::load();
    let port = port_override.unwrap_or(config.port);

    if config.api_key.is_empty() && config.providers.is_empty() {
        eprintln!(
            "WARNING: {} is not set and no config file providers found. LLM calls will fail.",
            match config.provider {
                Provider::Anthropic => "ANTHROPIC_API_KEY",
                Provider::OpenAI => "OPENAI_API_KEY",
            }
        );
    }

    eprintln!("Config:");
    eprintln!("  Provider:      {}", config.provider.label());
    eprintln!("  Model:         {}", config.model);
    eprintln!("  API base:      {}", config.api_base);
    if !config.providers.is_empty() {
        let names: Vec<&str> = config.providers.keys().map(|s| s.as_str()).collect();
        let total: usize = config.providers.values().map(|p| p.models.len()).sum();
        eprintln!("  Config providers: {} ({} models)", names.join(", "), total);
    }
    eprintln!("  Exec timeout:  {}s", config.exec_timeout.as_secs());
    eprintln!("  Max rounds:    {}", config.max_tool_rounds);
    eprintln!("  Context limit: {} tokens", config.max_context_tokens);

    let shutdown = CancellationToken::new();

    // Generate a one-time shutdown token and write it to disk for CLI use
    let shutdown_token: String = {
        use std::collections::hash_map::RandomState;
        use std::hash::{BuildHasher, Hasher};
        let mut h1 = RandomState::new().build_hasher();
        let mut h2 = RandomState::new().build_hasher();
        h1.write_u8(1);
        h2.write_u8(2);
        format!("{:016x}{:016x}", h1.finish(), h2.finish())
    };
    if let Some(dir) = config_dir_path() {
        let _ = std::fs::write(dir.join(format!("shutdown-{port}.token")), &shutdown_token);
    }

    let state = Arc::new(AppState {
        config,
        http: Client::new(),
        sessions: Mutex::new(HashMap::new()),
        shutdown: shutdown.clone(),
        shutdown_token,
    });

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/health", get(api_health))
        .route("/api/sessions", get(api_sessions))
        .route("/api/shutdown", post(api_shutdown))
        .fallback_service(ServeDir::new("static").append_index_html_on_directories(true))
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    let addr = format!("127.0.0.1:{port}");
    println!("🦀 LingClaw v2 listening on http://{addr}");
    println!("   Tools: think, exec, read_file, write_file, patch_file, list_dir, search_files, http_fetch");

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");

    let shutdown_signal = {
        let s = shutdown.clone();
        async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => { s.cancel(); },
                _ = s.cancelled() => {},
            }
        }
    };
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await
        .ok();

    // Flush all in-memory sessions to disk before exiting
    let sessions: Vec<Session> = {
        let guard = state.sessions.lock().await;
        guard.values().cloned().collect()
    };
    for s in &sessions {
        if s.messages.len() > 1 {
            let _ = save_session_to_disk(s).await;
        }
    }
    // Clean up shutdown token file
    if let Some(dir) = config_dir_path() {
        let _ = std::fs::remove_file(dir.join(format!("shutdown-{port}.token")));
    }
    eprintln!("Server shut down, {} session(s) saved.", sessions.len());
}
