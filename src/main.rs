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
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tower_http::services::ServeDir;

mod agent;
mod cli;
mod prompts;
mod providers;
mod tools;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const MAIN_SESSION_ID: &str = "main";
pub(crate) const DEFAULT_PORT: u16 = 18989;

// ── Config ──────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provider {
    OpenAI,
    Anthropic,
}

impl Provider {
    fn detect(model: &str, api_base: &str, json_provider: Option<&str>) -> Self {
        // Explicit override: env var > JSON settings > auto-detect
        let env_explicit = std::env::var("LINGCLAW_PROVIDER")
            .unwrap_or_default()
            .to_lowercase();
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
        if let Some((provider_name, model_id)) = model.split_once('/') {
            let provider_name = provider_name.to_lowercase();
            if provider_name == "anthropic" {
                return Self::Anthropic;
            }
            if provider_name == "openai" {
                return Self::OpenAI;
            }
            if model_id.starts_with("claude") {
                return Self::Anthropic;
            }
        }
        // Auto-detect from model name or API base
        if model.starts_with("claude") || api_base.contains("anthropic.com") {
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

        // API base: legacy settings.apiBase → env OPENAI_API_BASE → default
        let api_base = settings
            .api_base
            .clone()
            .or_else(|| std::env::var("OPENAI_API_BASE").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        let provider = Provider::detect(&model, &api_base, settings.provider.as_deref());

        // API key: legacy settings.apiKey → env vars → ""
        let api_key = settings.api_key.clone().unwrap_or_else(|| match provider {
            Provider::Anthropic => std::env::var("ANTHROPIC_API_KEY")
                .or_else(|_| std::env::var("OPENAI_API_KEY"))
                .unwrap_or_default(),
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
            port: settings
                .port
                .or_else(|| std::env::var("LINGCLAW_PORT").ok()?.parse().ok())
                .unwrap_or(DEFAULT_PORT),
            max_context_tokens: settings
                .max_context_tokens
                .or_else(|| {
                    std::env::var("LINGCLAW_MAX_CONTEXT_TOKENS")
                        .ok()?
                        .parse()
                        .ok()
                })
                .unwrap_or(32000),
            exec_timeout: Duration::from_secs(
                settings
                    .exec_timeout
                    .or_else(|| std::env::var("LINGCLAW_EXEC_TIMEOUT").ok()?.parse().ok())
                    .unwrap_or(30),
            ),
            max_output_bytes: settings.max_output_bytes.unwrap_or(50 * 1024),
            max_file_bytes: settings.max_file_bytes.unwrap_or(200 * 1024),
        }
    }

    /// Resolve a model reference ("provider/model" or plain "model-name") to
    /// a concrete provider, API base, API key, and model ID.
    fn resolve_model(&self, model_ref: &str) -> providers::ResolvedModel {
        let fallback_resolved = |provider: Provider, model_id: &str| providers::ResolvedModel {
            provider,
            api_base: match provider {
                Provider::Anthropic if self.api_base == "https://api.openai.com/v1" => {
                    "https://api.anthropic.com".to_string()
                }
                _ => self.api_base.clone(),
            },
            api_key: match provider {
                Provider::Anthropic if self.provider != Provider::Anthropic => {
                    std::env::var("ANTHROPIC_API_KEY")
                        .or_else(|_| std::env::var("OPENAI_API_KEY"))
                        .unwrap_or_else(|_| self.api_key.clone())
                }
                Provider::OpenAI if self.provider != Provider::OpenAI => {
                    std::env::var("OPENAI_API_KEY").unwrap_or_else(|_| self.api_key.clone())
                }
                _ => self.api_key.clone(),
            },
            model_id: model_id.to_string(),
            reasoning: false,
            thinking_format: None,
            max_tokens: None,
        };

        let build_resolved =
            |pc: &JsonProviderConfig, model_id: &str, entry: Option<&JsonModelEntry>| {
                let reasoning = entry.and_then(|e| e.reasoning).unwrap_or(false);
                let thinking_format = entry
                    .and_then(|e| e.compat.as_ref())
                    .and_then(|c| c.get("thinkingFormat"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let max_tokens = entry.and_then(|e| e.max_tokens);
                providers::ResolvedModel {
                    provider: match pc.api.as_str() {
                        "anthropic" => Provider::Anthropic,
                        _ => Provider::OpenAI,
                    },
                    api_base: pc.base_url.clone(),
                    api_key: pc.api_key.clone(),
                    model_id: model_id.to_string(),
                    reasoning,
                    thinking_format,
                    max_tokens,
                }
            };

        // Try "provider/model" format
        if let Some((prov_name, model_id)) = model_ref.split_once('/') {
            if let Some(pc) = self.providers.get(prov_name) {
                let entry = pc.models.iter().find(|m| m.id == model_id);
                return build_resolved(pc, model_id, entry);
            }
            if self.providers.is_empty() {
                let provider = match prov_name.to_ascii_lowercase().as_str() {
                    "anthropic" => Some(Provider::Anthropic),
                    "openai" => Some(Provider::OpenAI),
                    _ => None,
                };
                if let Some(provider) = provider {
                    return fallback_resolved(provider, model_id);
                }
            }
        }
        // Fallback: search configured providers by plain model id, preferring
        // an exact match to the current runtime config, then same provider
        // type, all with stable provider-name ordering.
        let mut provider_names: Vec<&str> =
            self.providers.keys().map(|name| name.as_str()).collect();
        provider_names.sort_unstable_by(|left, right| {
            let rank = |name: &str| {
                let pc = self
                    .providers
                    .get(name)
                    .expect("provider missing during sort");
                let pc_provider = match pc.api.as_str() {
                    "anthropic" => Provider::Anthropic,
                    _ => Provider::OpenAI,
                };
                if pc_provider == self.provider
                    && pc.base_url == self.api_base
                    && pc.api_key == self.api_key
                {
                    0_u8
                } else if pc_provider == self.provider && pc.base_url == self.api_base {
                    1_u8
                } else if pc_provider == self.provider {
                    2_u8
                } else {
                    3_u8
                }
            };

            rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
        });

        for name in &provider_names {
            let Some(pc) = self.providers.get(*name) else {
                continue;
            };
            if let Some(entry) = pc.models.iter().find(|m| m.id == model_ref) {
                return build_resolved(pc, model_ref, Some(entry));
            }
        }

        // Fallback to env-based config
        fallback_resolved(self.provider, model_ref)
    }

    /// List all available models: from config file providers + the default env model.
    fn available_models(&self) -> Vec<String> {
        let mut models: Vec<String> = Vec::new();
        for (prov_name, pc) in &self.providers {
            for m in &pc.models {
                models.push(format!("{prov_name}/{}", m.id));
            }
        }
        if models.is_empty() {
            models.push(self.model.clone());
        } else if let Ok(canonical) = self.canonical_model_ref(&self.model) {
            if !models.iter().any(|m| m == &canonical) {
                models.push(canonical);
            }
        }
        models
    }

    fn resolved_model_ref(&self, model_ref: &str) -> String {
        if let Some((prov_name, model_id)) = model_ref.split_once('/') {
            if self.providers.contains_key(prov_name) {
                return format!("{prov_name}/{model_id}");
            }
            if self.providers.is_empty() {
                let provider = prov_name.to_ascii_lowercase();
                if provider == "openai" || provider == "anthropic" {
                    return format!("{provider}/{model_id}");
                }
            }
        }

        let mut provider_names: Vec<&str> =
            self.providers.keys().map(|name| name.as_str()).collect();
        provider_names.sort_unstable_by(|left, right| {
            let rank = |name: &str| {
                let pc = self
                    .providers
                    .get(name)
                    .expect("provider missing during sort");
                let pc_provider = match pc.api.as_str() {
                    "anthropic" => Provider::Anthropic,
                    _ => Provider::OpenAI,
                };
                if pc_provider == self.provider
                    && pc.base_url == self.api_base
                    && pc.api_key == self.api_key
                {
                    0_u8
                } else if pc_provider == self.provider && pc.base_url == self.api_base {
                    1_u8
                } else if pc_provider == self.provider {
                    2_u8
                } else {
                    3_u8
                }
            };

            rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
        });

        for name in &provider_names {
            let Some(pc) = self.providers.get(*name) else {
                continue;
            };
            if pc.models.iter().any(|m| m.id == model_ref) {
                return format!("{name}/{model_ref}");
            }
        }

        model_ref.to_string()
    }

    fn canonical_model_ref(&self, model_ref: &str) -> Result<String, String> {
        let trimmed = model_ref.trim();
        if trimmed.is_empty() {
            return Err("Model name cannot be empty.".into());
        }

        if let Some((prov_name, model_id)) = trimmed.split_once('/') {
            if self.providers.is_empty() {
                let provider = prov_name.to_ascii_lowercase();
                if provider == "openai" || provider == "anthropic" {
                    return Ok(format!("{provider}/{model_id}"));
                }
                return Err(format!(
                    "Unknown provider '{prov_name}'. Use 'openai' or 'anthropic'."
                ));
            }
            let Some(pc) = self.providers.get(prov_name) else {
                return Err(format!(
                    "Unknown provider '{prov_name}'. Use /model to list available models."
                ));
            };
            if pc.models.iter().any(|m| m.id == model_id) {
                return Ok(format!("{prov_name}/{model_id}"));
            }
            return Err(format!(
                "Model '{model_id}' is not configured under provider '{prov_name}'."
            ));
        }

        let matches: Vec<String> = self
            .providers
            .iter()
            .filter(|(_, pc)| pc.models.iter().any(|m| m.id == trimmed))
            .map(|(prov_name, _)| format!("{prov_name}/{trimmed}"))
            .collect();

        match matches.len() {
            0 if self.providers.is_empty() => Ok(trimmed.to_string()),
            0 => Err(format!(
                "Unknown model '{trimmed}'. Use /model to list available models."
            )),
            1 => Ok(matches[0].clone()),
            _ => Err(format!(
                "Model '{trimmed}' is ambiguous. Use one of: {}",
                matches.join(", ")
            )),
        }
    }

    /// Look up the JsonModelEntry for a given model reference ("provider/model" or plain id).
    fn find_model_entry(&self, model_ref: &str) -> Option<&JsonModelEntry> {
        if let Some((prov_name, model_id)) = model_ref.split_once('/') {
            if let Some(pc) = self.providers.get(prov_name) {
                return pc.models.iter().find(|m| m.id == model_id);
            }
        }
        // Fallback: search all providers by plain id
        for pc in self.providers.values() {
            if let Some(entry) = pc.models.iter().find(|m| m.id == model_ref) {
                return Some(entry);
            }
        }
        None
    }

    /// Return the effective context token limit for the given model.
    /// Priority: model's contextWindow → settings.maxContextTokens → 32000.
    fn context_limit_for_model(&self, model_ref: &str) -> usize {
        if let Some(entry) = self.find_model_entry(model_ref) {
            if let Some(cw) = entry.context_window {
                return cw as usize;
            }
        }
        self.max_context_tokens
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

// ── Data Models ──────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    timestamp: Option<u64>,
}

impl ChatMessage {
    fn has_nonempty_content(&self) -> bool {
        self.content
            .as_deref()
            .is_some_and(|content| !content.is_empty())
    }

    fn has_tool_calls(&self) -> bool {
        self.tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
    }

    fn is_empty_assistant_message(&self) -> bool {
        self.role == "assistant" && !self.has_nonempty_content() && !self.has_tool_calls()
    }
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

// ── Session & AppState ───────────────────────────────────────────────────────────────────────

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

const SESSION_VERSION: u32 = 3;

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
    #[serde(default = "default_show_react")]
    show_react: bool,
    #[serde(default = "default_show_tools")]
    show_tools: bool,
    #[serde(default = "default_show_reasoning")]
    show_reasoning: bool,
    #[serde(default)]
    version: u32,
    #[serde(skip)]
    workspace: PathBuf,
    /// Avatar from IDENTITY.md (transient, not persisted)
    #[serde(skip)]
    avatar: Option<String>,
}

fn default_think_level() -> String {
    "auto".to_string()
}

fn default_show_react() -> bool {
    true
}

fn default_show_tools() -> bool {
    true
}

fn default_show_reasoning() -> bool {
    true
}

fn migrate_session(session: &mut Session) {
    if session.version < 2 {
        session.show_react = default_show_react();
    }
    if session.version < 3 {
        session.show_tools = default_show_tools();
        session.show_reasoning = default_show_reasoning();
    }
    session.version = SESSION_VERSION;
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
        Self::new_with_id(&gen_session_id(), "New Chat")
    }

    fn new_with_id(id: &str, name: &str) -> Self {
        let workspace = session_workspace_path(id);
        std::fs::create_dir_all(&workspace).ok();
        prompts::init_session_prompt_files(&workspace);
        let avatar = prompts::parse_identity_avatar(&workspace);
        Self {
            id: id.to_string(),
            name: name.to_string(),
            messages: Vec::new(),
            created_at: now_epoch(),
            updated_at: now_epoch(),
            tool_calls_count: 0,
            model_override: None,
            think_level: default_think_level(),
            show_react: default_show_react(),
            show_tools: default_show_tools(),
            show_reasoning: default_show_reasoning(),
            version: SESSION_VERSION,
            workspace,
            avatar,
        }
    }

    fn is_main(&self) -> bool {
        self.id == MAIN_SESSION_ID
    }

    fn effective_model<'a>(&'a self, default: &'a str) -> &'a str {
        self.model_override.as_deref().unwrap_or(default)
    }
}

struct AppState {
    config: Config,
    http: Client,
    sessions: Mutex<HashMap<String, Session>>,
    /// Session IDs with the connection currently attached to live streaming output.
    active_connections: Mutex<HashMap<String, u64>>,
    session_clients: Mutex<HashMap<String, SessionClientBinding>>,
    live_rounds: Mutex<HashMap<String, LiveRoundState>>,
    next_connection_id: AtomicU64,
    shutdown: CancellationToken,
    shutdown_token: String,
}

#[derive(Clone)]
struct SessionClientBinding {
    connection_id: u64,
    tx: WsTx,
    replay_ready: bool,
    pending_events: Vec<serde_json::Value>,
}

#[derive(Clone, Default)]
struct LiveToolState {
    id: String,
    name: String,
    arguments: String,
    result: Option<String>,
}

#[derive(Clone, Default)]
struct LiveRoundState {
    round: usize,
    avatar: Option<String>,
    react_visible: bool,
    phase: Option<String>,
    cycle: Option<usize>,
    assistant_text: String,
    reasoning_text: String,
    reasoning_done: bool,
    tools: Vec<LiveToolState>,
}

/// Cap for replay buffer strings (128 KB). Keeps memory bounded for long outputs.
const LIVE_REPLAY_CAP: usize = 128 * 1024;

// ── System Prompt ────────────────────────────────────────────────────────────

fn build_system_prompt(
    config: &Config,
    workspace: &Path,
    model: &str,
    is_main: bool,
) -> ChatMessage {
    let os_name = if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    let cwd = workspace.display();
    let local_snapshot = prompts::current_local_snapshot();
    let local_time = local_snapshot.datetime_label();
    let tool_lines = tools::render_tool_prompt_lines(config);
    let persona = prompts::load_session_prompt_files_with_snapshot(workspace, local_snapshot);
    let prompt_file_note = "## Preloaded Prompt Files\n\
These prompt-file contents were already loaded into this system prompt from the session workspace.\n\
Do not call file tools just to verify or re-read BOOTSTRAP.md, AGENTS.md, AGENT.md, IDENTITY.md, USER.md, SOUL.md, or MEMORY.md when their content is already present below.\n\
Only read those files if the user explicitly asks to inspect them, if you need to edit them, or if a task depends on checking whether the on-disk file has changed.";

    let admin_section = if is_main {
        "\n\n## Admin Tools (Main Session Only)\n\
         You have access to session management tools. When users ask about sessions, \
         session counts, or want to manage/delete sessions, use these tools directly.\n\
         - list_sessions: List all sessions with model, context usage, and configuration\n\
         - delete_session: Delete a session by ID (cannot delete the main session)"
    } else {
        ""
    };

    let prompt = format!(
        r#"{persona}

---

## Environment
- OS: {os_name}
- Current system local time: {local_time}
- Working directory: {cwd}
- Model: {model}

{prompt_file_note}

## Available Tools
{tool_lines}{admin_section}"#,
        model = model,
        local_time = local_time,
        tool_lines = tool_lines,
        persona = persona,
        prompt_file_note = prompt_file_note,
        admin_section = admin_section,
    );

    ChatMessage {
        role: "system".into(),
        content: Some(prompt),
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }
}

// ── Security ─────────────────────────────────────────────────────────────────────────────

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

#[cfg_attr(not(test), allow(dead_code))]
fn resolve_path(path_str: &str, workspace: &Path) -> PathBuf {
    let ws_canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let raw = Path::new(path_str);
    let relative = if raw.is_absolute() {
        match raw.strip_prefix(&ws_canonical) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => {
                eprintln!("SECURITY: path '{}' escapes workspace, clamped", path_str);
                return ws_canonical;
            }
        }
    } else {
        raw.to_path_buf()
    };

    let mut resolved = ws_canonical.clone();
    for comp in relative.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if resolved == ws_canonical {
                    eprintln!("SECURITY: path '{}' escapes workspace, clamped", path_str);
                    return ws_canonical;
                }
                resolved.pop();
            }
            std::path::Component::Normal(part) => {
                resolved.push(part);
                if let Ok(meta) = std::fs::symlink_metadata(&resolved) {
                    if meta.file_type().is_symlink() {
                        eprintln!(
                            "SECURITY: path '{}' traverses symlink '{}', clamped",
                            path_str,
                            resolved.display()
                        );
                        return ws_canonical;
                    }
                }
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                eprintln!(
                    "SECURITY: absolute path '{}' is not allowed, clamped",
                    path_str
                );
                return ws_canonical;
            }
        }
    }

    resolved
}

fn resolve_path_checked(path_str: &str, workspace: &Path) -> Result<PathBuf, String> {
    let workspace_root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let raw = Path::new(path_str);
    let relative = if raw.is_absolute() {
        if let Ok(relative) = raw.strip_prefix(workspace) {
            relative.to_path_buf()
        } else if let Ok(relative) = raw.strip_prefix(&workspace_root) {
            relative.to_path_buf()
        } else if let Ok(canonical_raw) = raw.canonicalize() {
            canonical_raw
                .strip_prefix(&workspace_root)
                .map(PathBuf::from)
                .map_err(|_| {
                    format!(
                        "path '{}' is outside the session workspace '{}'",
                        path_str,
                        workspace_root.display()
                    )
                })?
        } else {
            return Err(format!(
                "path '{}' is outside the session workspace '{}'",
                path_str,
                workspace_root.display()
            ));
        }
    } else {
        raw.to_path_buf()
    };

    let mut resolved = workspace_root.clone();
    for component in relative.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if resolved == workspace_root {
                    return Err(format!(
                        "path '{}' is outside the session workspace '{}'",
                        path_str,
                        workspace_root.display()
                    ));
                }
                resolved.pop();
            }
            std::path::Component::Normal(part) => {
                resolved.push(part);
                if let Ok(meta) = std::fs::symlink_metadata(&resolved) {
                    if meta.file_type().is_symlink() {
                        return Err(format!(
                            "path '{}' traverses symlink '{}' outside the session workspace '{}'",
                            path_str,
                            resolved.display(),
                            workspace_root.display()
                        ));
                    }
                }
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(format!(
                    "path '{}' is outside the session workspace '{}'",
                    path_str,
                    workspace_root.display()
                ));
            }
        }
    }

    Ok(resolved)
}

fn generate_shutdown_token() -> String {
    let mut bytes = [0_u8; 32];
    match getrandom::getrandom(&mut bytes) {
        Ok(()) => bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
        Err(e) => {
            eprintln!("WARNING: failed to get secure random bytes for shutdown token: {e}");
            let fallback = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                ^ u128::from(std::process::id());
            format!("{fallback:032x}{fallback:032x}")
        }
    }
}

fn find_static_dir_from(exe: Option<&Path>, cwd: Option<&Path>) -> PathBuf {
    if let Some(exe_path) = exe {
        for ancestor in exe_path.ancestors().skip(1).take(3) {
            let candidate = ancestor.join("static");
            if candidate.join("index.html").is_file() {
                return candidate;
            }
        }
    }

    if let Some(cwd_path) = cwd {
        let candidate = cwd_path.join("static");
        if candidate.join("index.html").is_file() {
            return candidate;
        }
    }

    PathBuf::from("static")
}

fn resolve_static_dir() -> PathBuf {
    let exe = std::env::current_exe().ok();
    let cwd = std::env::current_dir().ok();
    find_static_dir_from(exe.as_deref(), cwd.as_deref())
}

// ── Utilities ────────────────────────────────────────────────────────────────

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Find the last valid UTF-8 char boundary at or before `max`
        // to avoid panicking on multi-byte characters.
        let end = (0..=max)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        format!(
            "{}...\n[truncated at {} bytes, total {} bytes]",
            &s[..end],
            end,
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

pub(crate) type WsTx = mpsc::Sender<String>;
pub(crate) type LiveTx = mpsc::Sender<serde_json::Value>;

pub(crate) async fn ws_send(tx: &WsTx, data: &serde_json::Value) -> bool {
    tx.send(data.to_string()).await.is_ok()
}

pub(crate) async fn live_send(tx: &LiveTx, data: serde_json::Value) -> bool {
    tx.send(data).await.is_ok()
}

fn ws_try_send(tx: &WsTx, data: &serde_json::Value) -> bool {
    tx.try_send(data.to_string()).is_ok()
}

async fn bind_session_connection(
    state: &AppState,
    session_id: &str,
    connection_id: u64,
    tx: &WsTx,
    replay_ready: bool,
) {
    state
        .active_connections
        .lock()
        .await
        .insert(session_id.to_string(), connection_id);
    state.session_clients.lock().await.insert(
        session_id.to_string(),
        SessionClientBinding {
            connection_id,
            tx: tx.clone(),
            replay_ready,
            pending_events: Vec::new(),
        },
    );
}

async fn finish_session_replay(state: &AppState, session_id: &str, connection_id: u64) {
    let (tx, pending_events) = {
        let mut clients = state.session_clients.lock().await;
        let Some(binding) = clients.get_mut(session_id) else {
            return;
        };
        if binding.connection_id != connection_id {
            return;
        }

        binding.replay_ready = true;
        (
            binding.tx.clone(),
            std::mem::take(&mut binding.pending_events),
        )
    };

    for event in pending_events {
        if !ws_send(&tx, &event).await {
            unbind_session_connection_if_matches(state, session_id, connection_id).await;
            break;
        }
    }
}

async fn unbind_session_connection_if_matches(
    state: &AppState,
    session_id: &str,
    connection_id: u64,
) {
    {
        let mut active = state.active_connections.lock().await;
        if active.get(session_id).copied() == Some(connection_id) {
            active.remove(session_id);
        }
    }

    let mut clients = state.session_clients.lock().await;
    if clients.get(session_id).map(|binding| binding.connection_id) == Some(connection_id) {
        clients.remove(session_id);
    }
}

async fn dispatch_live_event(state: &AppState, session_id: &str, event: serde_json::Value) {
    let event_type = event["type"].as_str().unwrap_or_default();

    {
        let mut live_rounds = state.live_rounds.lock().await;
        match event_type {
            "start" => {
                live_rounds.insert(
                    session_id.to_string(),
                    LiveRoundState {
                        round: event["round"].as_u64().unwrap_or(1) as usize,
                        avatar: event["avatar"].as_str().map(str::to_string),
                        react_visible: event["react_visible"].as_bool().unwrap_or(false),
                        phase: event["phase"].as_str().map(str::to_string),
                        cycle: event["cycle"].as_u64().map(|value| value as usize),
                        assistant_text: String::new(),
                        reasoning_text: String::new(),
                        reasoning_done: false,
                        tools: Vec::new(),
                    },
                );
            }
            "delta" => {
                if let Some(round) = live_rounds.get_mut(session_id) {
                    if let Some(content) = event["content"].as_str() {
                        if round.assistant_text.len() < LIVE_REPLAY_CAP {
                            round.assistant_text.push_str(content);
                            round.assistant_text.truncate(LIVE_REPLAY_CAP);
                        }
                    }
                }
            }
            "thinking_start" => {
                if let Some(round) = live_rounds.get_mut(session_id) {
                    round.reasoning_text.clear();
                    round.reasoning_done = false;
                }
            }
            "thinking_delta" => {
                if let Some(round) = live_rounds.get_mut(session_id) {
                    if let Some(content) = event["content"].as_str() {
                        if round.reasoning_text.len() < LIVE_REPLAY_CAP {
                            round.reasoning_text.push_str(content);
                            round.reasoning_text.truncate(LIVE_REPLAY_CAP);
                        }
                    }
                }
            }
            "thinking_done" => {
                if let Some(round) = live_rounds.get_mut(session_id) {
                    round.reasoning_done = true;
                }
            }
            "tool_call" => {
                if let Some(round) = live_rounds.get_mut(session_id) {
                    round.tools.push(LiveToolState {
                        id: event["id"].as_str().unwrap_or_default().to_string(),
                        name: event["name"].as_str().unwrap_or_default().to_string(),
                        arguments: event["arguments"].as_str().unwrap_or_default().to_string(),
                        result: None,
                    });
                }
            }
            "tool_result" => {
                if let Some(round) = live_rounds.get_mut(session_id) {
                    let tool_id = event["id"].as_str().unwrap_or_default();
                    let mut result = event["result"].as_str().unwrap_or_default().to_string();
                    result.truncate(LIVE_REPLAY_CAP);
                    if let Some(tool) = round.tools.iter_mut().find(|tool| tool.id == tool_id) {
                        tool.result = Some(result);
                    } else {
                        round.tools.push(LiveToolState {
                            id: tool_id.to_string(),
                            name: event["name"].as_str().unwrap_or_default().to_string(),
                            arguments: String::new(),
                            result: Some(result),
                        });
                    }
                }
            }
            "react_phase" => {
                if let Some(round) = live_rounds.get_mut(session_id) {
                    round.phase = event["phase"].as_str().map(str::to_string);
                    round.cycle = event["cycle"].as_u64().map(|value| value as usize);
                }
            }
            "done" | "error" => {
                live_rounds.remove(session_id);
            }
            _ => {}
        }
    }

    let binding = {
        let mut clients = state.session_clients.lock().await;
        if let Some(binding) = clients.get_mut(session_id) {
            if !binding.replay_ready {
                binding.pending_events.push(event.clone());
                None
            } else {
                Some(binding.clone())
            }
        } else {
            None
        }
    };
    if let Some(binding) = binding {
        if !ws_send(&binding.tx, &event).await {
            unbind_session_connection_if_matches(state, session_id, binding.connection_id).await;
        }
    }
}

async fn replay_live_round(tx: &WsTx, state: &AppState, session_id: &str) {
    let live_round = { state.live_rounds.lock().await.get(session_id).cloned() };
    let Some(live_round) = live_round else {
        return;
    };

    ws_send(
        tx,
        &json!({
            "type":"start",
            "round": live_round.round,
            "avatar": live_round.avatar,
            "phase": live_round.phase.as_deref().unwrap_or("analyze"),
            "cycle": live_round.cycle,
            "react_visible": live_round.react_visible,
        }),
    )
    .await;

    if !live_round.reasoning_text.is_empty() {
        ws_send(tx, &json!({"type":"thinking_start"})).await;
        ws_send(
            tx,
            &json!({"type":"thinking_delta","content": live_round.reasoning_text}),
        )
        .await;
        if live_round.reasoning_done {
            ws_send(tx, &json!({"type":"thinking_done"})).await;
        }
    }

    for tool in &live_round.tools {
        ws_send(
            tx,
            &json!({
                "type":"tool_call",
                "id": tool.id,
                "name": tool.name,
                "arguments": tool.arguments,
            }),
        )
        .await;
        if let Some(result) = &tool.result {
            ws_send(
                tx,
                &json!({
                    "type":"tool_result",
                    "id": tool.id,
                    "name": tool.name,
                    "result": result,
                }),
            )
            .await;
        }
    }

    if !live_round.assistant_text.is_empty() {
        ws_send(
            tx,
            &json!({"type":"delta","content": live_round.assistant_text}),
        )
        .await;
    }
}

fn build_agent_hard_cap_events(
    round_limit: usize,
    cycles: usize,
    tool_calls: usize,
) -> (serde_json::Value, serde_json::Value) {
    (
        json!({
            "type": "system",
            "content": format!(
                "Detected abnormal tool loop ({} consecutive rounds). Stopping.",
                round_limit
            ),
        }),
        json!({
            "type": "done",
            "phase": "hard_cap",
            "reason": "hard_cap",
            "cycles": cycles,
            "tool_calls": tool_calls,
        }),
    )
}

async fn detect_session_avatar_update(
    session_id: &str,
    state: &AppState,
) -> Option<Option<String>> {
    let (workspace, current_avatar) = {
        let sessions = state.sessions.lock().await;
        let session = sessions.get(session_id)?;
        (session.workspace.clone(), session.avatar.clone())
    };
    let fresh = prompts::parse_identity_avatar(&workspace);
    if fresh != current_avatar {
        Some(fresh)
    } else {
        None
    }
}

async fn commit_session_avatar(session_id: &str, avatar: Option<String>, state: &AppState) {
    let mut sessions = state.sessions.lock().await;
    if let Some(session) = sessions.get_mut(session_id) {
        session.avatar = avatar;
    }
}

// ── Tool Dispatch ────────────────────────────────────────────────────────────

async fn execute_tool(
    name: &str,
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
) -> tools::ToolOutcome {
    tools::execute_tool(name, args_str, config, http, workspace).await
}

// ── Context Management ──────────────────────────────────────────────────────

fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages.iter().map(message_token_len).sum()
}

fn message_token_len(message: &ChatMessage) -> usize {
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
fn turn_len(messages: &[ChatMessage], start: usize) -> usize {
    let msg = &messages[start];
    if msg.role == "user" {
        // Remove the user message together with its following assistant reply,
        // if present, so we prune complete conversational turns.
        if start + 1 < messages.len() {
            let next = &messages[start + 1];
            if next.role == "assistant" {
                if let Some(tcs) = &next.tool_calls {
                    if !tcs.is_empty() {
                        let tool_results = messages[start + 2..]
                            .iter()
                            .take_while(|m| m.role == "tool")
                            .count();
                        return 2 + tool_results; // user + assistant + tool results
                    }
                }
                return 2; // user + assistant text reply
            }
        }
        return 1; // standalone user
    }
    if msg.role == "assistant" {
        if let Some(tcs) = &msg.tool_calls {
            if !tcs.is_empty() {
                let tool_results = messages[start + 1..]
                    .iter()
                    .take_while(|m| m.role == "tool")
                    .count();
                return 1 + tool_results; // assistant + tool results
            }
        }
    }
    1 // standalone assistant or tool message
}

fn prune_messages(messages: &mut Vec<ChatMessage>, max_tokens: usize) {
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

// ── Session Persistence ──────────────────────────────────────────────────────

fn sessions_dir() -> PathBuf {
    let dir = config_dir_path()
        .unwrap_or_else(|| PathBuf::from(".lingclaw"))
        .join("sessions");
    std::fs::create_dir_all(&dir).ok();
    dir
}

async fn save_session_to_disk(session: &Session) -> Result<(), String> {
    let path = sessions_dir().join(format!("{}.json", session.id));
    let tmp_path = sessions_dir().join(format!("{}.json.tmp", session.id));
    let mut session = session.clone();
    sanitize_session_messages(&mut session.messages);
    let data = serde_json::to_string_pretty(&session).map_err(|e| e.to_string())?;
    // Write through a temp file first. On Unix rename is atomic; on Windows we
    // must remove the old target before renaming because overwrite is rejected.
    tokio::fs::write(&tmp_path, data)
        .await
        .map_err(|e| e.to_string())?;

    #[cfg(windows)]
    if tokio::fs::try_exists(&path)
        .await
        .map_err(|e| e.to_string())?
    {
        tokio::fs::remove_file(&path)
            .await
            .map_err(|e| e.to_string())?;
    }

    tokio::fs::rename(&tmp_path, &path)
        .await
        .map_err(|e| e.to_string())
}

fn sanitize_session_messages(messages: &mut Vec<ChatMessage>) {
    messages.retain(|message| !message.is_empty_assistant_message());
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
    let Some(idx) = ast_idx else {
        sanitize_session_messages(messages);
        return;
    };
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

    sanitize_session_messages(messages);
}

fn load_session_from_disk(id: &str) -> Option<Session> {
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return None;
    }
    let path = sessions_dir().join(format!("{id}.json"));
    let data = std::fs::read_to_string(&path).ok()?;
    let mut session: Session = serde_json::from_str(&data).ok()?;
    migrate_session(&mut session);
    trim_incomplete_tool_calls(&mut session.messages);
    session.workspace = session_workspace_path(&session.id);
    std::fs::create_dir_all(&session.workspace).ok();
    prompts::ensure_session_workspace(&session.workspace);
    session.avatar = prompts::parse_identity_avatar(&session.workspace);
    Some(session)
}

fn refresh_session_system_prompt(state: &AppState, session: &mut Session) {
    let model = session.effective_model(&state.config.model).to_string();
    let sys = build_system_prompt(&state.config, &session.workspace, &model, session.is_main());
    if let Some(first) = session.messages.first_mut() {
        if first.role == "system" {
            *first = sys;
        }
    }
}

enum ClaimSessionResult {
    Claimed(String),
    InUse,
    NotFound,
}

async fn try_claim_session(id: &str, state: &AppState, connection_id: u64) -> ClaimSessionResult {
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return ClaimSessionResult::NotFound;
    }

    // Phase 1: quick check — is there an active connection?
    if state.active_connections.lock().await.contains_key(id) {
        return ClaimSessionResult::InUse;
    }

    // Phase 2: try claiming from in-memory orphan (no disk I/O)
    {
        let mut active = state.active_connections.lock().await;
        if active.contains_key(id) {
            return ClaimSessionResult::InUse;
        }
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(id) {
            refresh_session_system_prompt(state, session);
            active.insert(id.to_string(), connection_id);
            return ClaimSessionResult::Claimed(id.to_string());
        }
    }

    // Phase 3: load from disk WITHOUT holding any lock
    let Some(mut session) = load_session_from_disk(id) else {
        return ClaimSessionResult::NotFound;
    };
    refresh_session_system_prompt(state, &mut session);

    // Phase 4: re-acquire locks and atomically claim
    let mut active = state.active_connections.lock().await;
    if active.contains_key(id) {
        return ClaimSessionResult::InUse;
    }
    let mut sessions = state.sessions.lock().await;
    if sessions.contains_key(id) {
        // Someone else loaded it while we were reading disk
        return ClaimSessionResult::InUse;
    }

    let sid = session.id.clone();
    sessions.insert(sid.clone(), session);
    active.insert(sid.clone(), connection_id);
    ClaimSessionResult::Claimed(sid)
}

fn list_saved_session_summaries_in_dir(dir: &Path) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
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
                            "corrupt": false,
                        }));
                    } else if let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) {
                        out.push(json!({
                            "id": id,
                            "name": "[Corrupt Session]",
                            "messages": 0,
                            "created_at": 0,
                            "updated_at": 0,
                            "corrupt": true,
                        }));
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| {
        let b_ts = b["updated_at"].as_u64().unwrap_or(0);
        let a_ts = a["updated_at"].as_u64().unwrap_or(0);
        b_ts.cmp(&a_ts)
    });
    out
}

/// List all saved session summaries from disk, sorted by updated_at desc.
fn list_saved_session_summaries() -> Vec<serde_json::Value> {
    list_saved_session_summaries_in_dir(&sessions_dir())
}

fn list_saved_session_ids_in_dir(dir: &Path) -> HashSet<String> {
    let mut ids = HashSet::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                ids.insert(stem.to_string());
            }
        }
    }
    ids
}

fn list_saved_session_ids() -> HashSet<String> {
    list_saved_session_ids_in_dir(&sessions_dir())
}

fn build_history_payload(session: &Session) -> serde_json::Value {
    let mut msgs = Vec::new();
    for msg in &session.messages {
        match msg.role.as_str() {
            "system" => {}
            "user" => {
                if let Some(c) = &msg.content {
                    msgs.push(json!({"role":"user","content":c,"timestamp":msg.timestamp}));
                }
            }
            "assistant" => {
                if let Some(c) = &msg.content {
                    if !c.is_empty() {
                        msgs.push(
                            json!({"role":"assistant","content":c,"timestamp":msg.timestamp}),
                        );
                    }
                }
                if let Some(tcs) = &msg.tool_calls {
                    if session.show_tools {
                        for tc in tcs {
                            msgs.push(json!({"role":"tool_call","name":tc.function.name,"arguments":tc.function.arguments,"id":tc.id}));
                        }
                    }
                }
            }
            "tool" => {
                if session.show_tools {
                    if let Some(c) = &msg.content {
                        msgs.push(json!({"role":"tool_result","result":c,"id":msg.tool_call_id.as_deref().unwrap_or("")}));
                    }
                }
            }
            _ => {}
        }
    }
    json!({"type":"history","messages":msgs})
}

fn build_view_state_payload(session: &Session) -> serde_json::Value {
    json!({
        "type": "view_state",
        "show_tools": session.show_tools,
        "show_reasoning": session.show_reasoning,
        "show_react": session.show_react,
    })
}

// ── Admin Helpers (Main Session) ─────────────────────────────────────────────

fn resolve_session_target(target: &str, known_ids: &HashSet<String>) -> Result<String, String> {
    if known_ids.contains(target) {
        return Ok(target.to_string());
    }

    let mut matches: Vec<&String> = known_ids
        .iter()
        .filter(|id| id.starts_with(target))
        .collect();
    matches.sort_unstable();
    match matches.len() {
        0 => Err(format!("Session '{}' not found.", target)),
        1 => Ok(matches[0].to_string()),
        _ => Err(format!(
            "Session '{}' is ambiguous. Use a longer ID.",
            target
        )),
    }
}

fn build_active_session_lines(
    sessions: &HashMap<String, Session>,
    active_ids: &HashSet<String>,
    config: &Config,
) -> Vec<String> {
    let mut ids: Vec<&String> = active_ids.iter().collect();
    ids.sort_unstable();

    ids.into_iter()
        .filter_map(|id| {
            let session = sessions.get(id)?;
            let model = session.effective_model(&config.model).to_string();
            let ctx_limit = config.context_limit_for_model(&model);
            let resolved = config.resolve_model(&model);
            let estimated = estimate_tokens(&session.messages);
            let mt_str = resolved
                .max_tokens
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".into());
            Some(format!(
                "  {id}  {}\n    model: {model}  context_est: {estimated}/{ctx_limit}  max_tokens: {mt_str}  [active]",
                session.name,
            ))
        })
        .collect()
}

fn build_session_status(session: &Session, config: &Config) -> String {
    let model_ref = session.effective_model(&config.model);
    let canonical_model = config
        .canonical_model_ref(model_ref)
        .unwrap_or_else(|_| model_ref.to_string());
    let resolved = config.resolve_model(&canonical_model);
    let ctx_limit = config.context_limit_for_model(&canonical_model);
    let estimated_tokens = estimate_tokens(&session.messages);
    let model_max_tokens = resolved
        .max_tokens
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".into());

    format!(
        "agent: LingClaw\n\
         model: {canonical_model}\n\
         resolved_provider: {}\n\
         resolved_api_base: {}\n\
         resolved_model_id: {}\n\
         max_tokens: {model_max_tokens}\n\
         context_est: {estimated_tokens}/{ctx_limit}\n\
         think: {}\n\
         react: {}\n\
         tools: {}\n\
         reasoning: {}",
        resolved.provider.label(),
        resolved.api_base,
        resolved.model_id,
        session.think_level,
        if session.show_react { "on" } else { "off" },
        if session.show_tools { "on" } else { "off" },
        if session.show_reasoning { "on" } else { "off" },
    )
}

/// Gather all sessions info for /sessions command and list_sessions tool.
async fn gather_sessions_status(state: &AppState) -> String {
    let active_ids: HashSet<String> = state
        .active_connections
        .lock()
        .await
        .keys()
        .cloned()
        .collect();

    let lines = {
        let sessions = state.sessions.lock().await;
        build_active_session_lines(&sessions, &active_ids, &state.config)
    };

    if lines.is_empty() {
        "No active sessions.".to_string()
    } else {
        format!("Active sessions ({}):\n{}", lines.len(), lines.join("\n"))
    }
}

/// Delete a session by ID. Returns a status message.
async fn delete_session_by_id(target: &str, state: &AppState) -> String {
    let target = target.trim();
    if target == MAIN_SESSION_ID {
        return "Cannot delete the main session.".to_string();
    }
    if target.contains('/') || target.contains('\\') || target.contains("..") {
        return "Invalid session ID.".to_string();
    }

    let known_ids: HashSet<String> = {
        let mut ids = {
            let sessions = state.sessions.lock().await;
            sessions.keys().cloned().collect::<HashSet<_>>()
        };
        ids.extend(list_saved_session_ids());
        ids
    };

    let resolved_id = match resolve_session_target(target, &known_ids) {
        Ok(id) => id,
        Err(message) => return message,
    };

    if resolved_id == MAIN_SESSION_ID {
        return "Cannot delete the main session.".to_string();
    }

    let active = state.active_connections.lock().await;
    if active.contains_key(&resolved_id) {
        return format!("Session '{}' is currently in use.", resolved_id);
    }

    let removed_session = {
        let mut sessions = state.sessions.lock().await;
        sessions.remove(&resolved_id)
    };

    let path = sessions_dir().join(format!("{resolved_id}.json"));
    let existed_on_disk = path.exists();
    if existed_on_disk {
        if let Err(e) = std::fs::remove_file(&path) {
            if let Some(session) = removed_session {
                state
                    .sessions
                    .lock()
                    .await
                    .insert(resolved_id.clone(), session);
            }
            return format!("Failed to delete session file: {e}");
        }
    }

    if removed_session.is_none() && !existed_on_disk {
        return format!("Session '{}' not found.", target);
    }

    // Optionally clean up workspace directory
    let ws_path = session_workspace_path(&resolved_id);
    if let Some(session_dir) = ws_path.parent() {
        if session_dir.exists() {
            let _ = std::fs::remove_dir_all(session_dir);
        }
    }

    format!("Deleted session '{}'.", resolved_id)
}

/// Admin tool definitions for the LLM (OpenAI format).
fn admin_tool_definitions_openai() -> Vec<serde_json::Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "list_sessions",
                "description": "List all sessions with their model, context usage, max_tokens, and status",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "delete_session",
                "description": "Delete a session by its ID. Cannot delete the main session or an active session.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "The session ID to delete"
                        }
                    },
                    "required": ["session_id"]
                }
            }
        }),
    ]
}

/// Admin tool definitions for the LLM (Anthropic format).
fn admin_tool_definitions_anthropic() -> Vec<serde_json::Value> {
    vec![
        json!({
            "name": "list_sessions",
            "description": "List all sessions with their model, context usage, max_tokens, and status",
            "input_schema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
        json!({
            "name": "delete_session",
            "description": "Delete a session by its ID. Cannot delete the main session or an active session.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The session ID to delete"
                    }
                },
                "required": ["session_id"]
            }
        }),
    ]
}

/// Execute an admin tool call. Returns the tool result string.
async fn execute_admin_tool(name: &str, args_str: &str, state: &AppState) -> String {
    match name {
        "list_sessions" => gather_sessions_status(state).await,
        "delete_session" => {
            let args: serde_json::Value = serde_json::from_str(args_str).unwrap_or_default();
            let session_id = args["session_id"].as_str().unwrap_or_default();
            if session_id.is_empty() {
                return "Error: session_id is required.".to_string();
            }
            delete_session_by_id(session_id, state).await
        }
        _ => format!("Unknown admin tool: {name}"),
    }
}

fn is_admin_tool(name: &str) -> bool {
    matches!(name, "list_sessions" | "delete_session")
}

// ── Chat Commands ────────────────────────────────────────────────────────────

struct CommandResult {
    response: String,
    response_type: &'static str,
    new_session_id: Option<String>,
    sessions_changed: bool,
}

async fn handle_command(
    input: &str,
    current_session_id: &str,
    connection_id: u64,
    state: &AppState,
    tx: &WsTx,
    cancel: &CancellationToken,
) -> Option<CommandResult> {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    async fn persist_session_toggle<F>(
        state: &AppState,
        current_session_id: &str,
        update: F,
    ) -> Result<(), String>
    where
        F: FnOnce(&mut Session),
    {
        let (previous_session, session_to_save) = {
            let mut sessions = state.sessions.lock().await;
            let session = sessions
                .get_mut(current_session_id)
                .ok_or_else(|| "Session not found".to_string())?;
            let previous = session.clone();
            update(session);
            (previous, session.clone())
        };

        if let Err(err) = save_session_to_disk(&session_to_save).await {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                *session = previous_session;
            }
            return Err(err);
        }

        Ok(())
    }

    match cmd {
        "/new" => {
            // Compress conversation → save to memory → clear context
            let (conversation_text, workspace, model_str) = {
                let sessions = state.sessions.lock().await;
                let session = match sessions.get(current_session_id) {
                    Some(s) => s,
                    None => {
                        return Some(CommandResult {
                            response: "Session not found".into(),
                            response_type: "system",
                            new_session_id: None,
                            sessions_changed: false,
                        })
                    }
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
                (
                    lines.join("\n"),
                    session.workspace.clone(),
                    session.effective_model(&state.config.model).to_string(),
                )
            };

            if conversation_text.is_empty() {
                // Nothing to compress — just clear
                let mut sessions = state.sessions.lock().await;
                if let Some(session) = sessions.get_mut(current_session_id) {
                    let model = session.effective_model(&state.config.model).to_string();
                    let is_main = session.is_main();
                    let sys =
                        build_system_prompt(&state.config, &session.workspace, &model, is_main);
                    session.messages = vec![sys];
                    session.tool_calls_count = 0;
                    session.updated_at = now_epoch();
                }
                return Some(CommandResult {
                    response: "Context cleared.".into(),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }

            if !ws_send(
                tx,
                &json!({
                    "type": "progress",
                    "content": "Compressing conversation..."
                }),
            )
            .await
            {
                return None;
            }

            // Ask LLM to compress
            let compress_prompt = vec![
                ChatMessage {
                    role: "system".into(),
                    content: Some("You are a conversation summarizer. Compress the following conversation into a concise markdown summary. Keep key decisions, code changes, problems solved, and important context. Use bullet points. Write in the same language as the conversation. Do NOT wrap in code blocks.".into()),
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: None,
                },
                ChatMessage {
                    role: "user".into(),
                    // Cap conversation text to avoid blowing the compression model's context.
                    content: Some(truncate(&conversation_text, 60_000)),
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: Some(now_epoch()),
                },
            ];
            let resolved = state.config.resolve_model(&model_str);
            let summary = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return Some(CommandResult {
                        response: "Shutdown: compression skipped, context unchanged.".into(),
                        response_type: "system",
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
                                response_type: "system",
                                new_session_id: None,
                                sessions_changed: false,
                            });
                        }
                    }
                }
            };

            if !ws_send(
                tx,
                &json!({
                    "type": "progress",
                    "content": "Compression complete. Writing memory..."
                }),
            )
            .await
            {
                return None;
            }

            // Append to memory/YYYY-MM-DD.md
            let local_snapshot = prompts::current_local_snapshot();
            let today = local_snapshot.today();
            let memory_dir = workspace.join("memory");
            std::fs::create_dir_all(&memory_dir).ok();
            let memory_path = memory_dir.join(format!("{today}.md"));

            let entry = {
                let local_time = local_snapshot.hhmm();
                format!(
                    "\n\n---\n\n## {local_time} Local\n\n{summary}",
                    summary = summary.trim()
                )
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
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }

            // Clear context
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                let model = session.effective_model(&state.config.model).to_string();
                let is_main = session.is_main();
                let sys = build_system_prompt(&state.config, &session.workspace, &model, is_main);
                session.messages = vec![sys];
                session.tool_calls_count = 0;
                session.updated_at = now_epoch();
            }

            Some(CommandResult {
                response: format!(
                    "Conversation compressed and saved to memory/{today}.md. Context cleared."
                ),
                response_type: "success",
                new_session_id: None,
                sessions_changed: false,
            })
        }

        "/session_new" => {
            // Save current session to disk BEFORE removing from memory
            let snapshot = {
                let sessions = state.sessions.lock().await;
                sessions.get(current_session_id).cloned()
            };
            if let Some(ref s) = snapshot {
                if s.messages.len() > 1 {
                    match save_session_to_disk(s).await {
                        Ok(()) => {
                            state.sessions.lock().await.remove(current_session_id);
                        }
                        Err(e) => {
                            eprintln!("Warning: failed to save session {} before /session_new: {e}; keeping in memory", s.id);
                        }
                    }
                } else {
                    state.sessions.lock().await.remove(current_session_id);
                }
            }
            // Create brand-new session
            let mut s = Session::new();
            let model = s.effective_model(&state.config.model).to_string();
            let sys = build_system_prompt(&state.config, &s.workspace, &model, false);
            s.messages.push(sys);
            let new_id = s.id.clone();
            state.sessions.lock().await.insert(new_id.clone(), s);
            Some(CommandResult {
                response: "A new journey begins.".into(),
                response_type: "system",
                new_session_id: Some(new_id),
                sessions_changed: true,
            })
        }

        "/switch" => {
            if arg.is_empty() {
                return Some(CommandResult {
                    response: "Usage: /switch <session_id>".into(),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let target = arg.to_string();
            if target == current_session_id {
                return Some(CommandResult {
                    response: "Already on this session.".into(),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            // Save current session to disk BEFORE removing (avoid data loss on save failure)
            let snapshot = {
                let sessions = state.sessions.lock().await;
                sessions.get(current_session_id).cloned()
            };
            if let Some(ref s) = snapshot {
                if s.messages.len() > 1 {
                    if let Err(e) = save_session_to_disk(s).await {
                        eprintln!("Warning: failed to save session {} before /switch: {e}; keeping in memory", s.id);
                        return Some(CommandResult {
                            response: "Failed to save current session; switch cancelled to avoid data loss.".into(),
                            response_type: "system",
                            new_session_id: None,
                            sessions_changed: false,
                        });
                    }
                }
            }
            match try_claim_session(&target, state, connection_id).await {
                ClaimSessionResult::Claimed(id) => {
                    // Claim succeeded — safe to remove old session from memory
                    state.sessions.lock().await.remove(current_session_id);
                    Some(CommandResult {
                        response: format!("Loaded session {}", &id[..12.min(id.len())]),
                        response_type: "system",
                        new_session_id: Some(id),
                        sessions_changed: true,
                    })
                }
                ClaimSessionResult::InUse => Some(CommandResult {
                    response: format!(
                        "Session '{}' is in use by another connection.",
                        &target[..12.min(target.len())]
                    ),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                }),
                ClaimSessionResult::NotFound => Some(CommandResult {
                    response: format!("Session '{}' not found.", &target[..12.min(target.len())]),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                }),
            }
        }

        "/rename" => {
            if arg.is_empty() {
                return Some(CommandResult {
                    response: "Usage: /rename <new_name>".into(),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                session.name = arg.to_string();
                Some(CommandResult {
                    response: format!("Renamed to: {arg}"),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: true,
                })
            } else {
                Some(CommandResult {
                    response: "Current session not found".into(),
                    response_type: "system",
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
                    .unwrap_or(&state.config.model)
                    .to_string();
                let current = state
                    .config
                    .canonical_model_ref(&model)
                    .unwrap_or(model.clone());
                let available = state.config.available_models();
                let list = available
                    .iter()
                    .map(|m| {
                        if m == &current {
                            format!("  * {m} (current)")
                        } else {
                            format!("    {m}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(CommandResult {
                    response: format!("Available models:\n{list}\n\nUse /model <name> to switch."),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                })
            } else {
                let canonical = match state.config.canonical_model_ref(arg) {
                    Ok(value) => value,
                    Err(err) => {
                        return Some(CommandResult {
                            response: err,
                            response_type: "error",
                            new_session_id: None,
                            sessions_changed: false,
                        })
                    }
                };
                let mut sessions = state.sessions.lock().await;
                if let Some(session) = sessions.get_mut(current_session_id) {
                    session.model_override = Some(canonical.clone());
                    Some(CommandResult {
                        response: format!("Model switched to: {canonical}"),
                        response_type: "system",
                        new_session_id: None,
                        sessions_changed: true,
                    })
                } else {
                    Some(CommandResult {
                        response: "Session not found".into(),
                        response_type: "system",
                        new_session_id: None,
                        sessions_changed: false,
                    })
                }
            }
        }

        "/status" => {
            let sessions = state.sessions.lock().await;
            match sessions.get(current_session_id) {
                Some(s) => Some(CommandResult {
                    response: build_session_status(s, &state.config),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                }),
                None => Some(CommandResult {
                    response: "No active session".into(),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                }),
            }
        }

        "/clear" => {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                let model = session.effective_model(&state.config.model).to_string();
                let is_main = session.is_main();
                let system_msg =
                    build_system_prompt(&state.config, &session.workspace, &model, is_main);
                session.messages = vec![system_msg];
                session.tool_calls_count = 0;
                session.updated_at = now_epoch();
                Some(CommandResult {
                    response: "Session cleared. System prompt preserved.".into(),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                })
            } else {
                Some(CommandResult {
                    response: "Session not found".into(),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                })
            }
        }

        "/skills" => {
            let list = tools::tool_specs()
                .iter()
                .map(|spec| {
                    let short = spec
                        .description
                        .split('.')
                        .next()
                        .unwrap_or(spec.description);
                    format!("  {} → {}", spec.name, short)
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(CommandResult {
                response: format!("Skills:\n{list}"),
                response_type: "system",
                new_session_id: None,
                sessions_changed: false,
            })
        }

        "/think" => {
            const VALID_LEVELS: &[&str] =
                &["auto", "off", "minimal", "low", "medium", "high", "xhigh"];
            if arg.is_empty() {
                let sessions = state.sessions.lock().await;
                let level = sessions
                    .get(current_session_id)
                    .map(|s| s.think_level.as_str())
                    .unwrap_or("auto");
                return Some(CommandResult {
                    response: format!(
                        "think: {level}\nUsage: /think <auto|off|minimal|low|medium|high|xhigh>"
                    ),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let level = arg.to_lowercase();
            if !VALID_LEVELS.contains(&level.as_str()) {
                return Some(CommandResult {
                    response: format!("Invalid think level: {arg}\nValid: auto, off, minimal, low, medium, high, xhigh"),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                session.think_level = level.clone();
                Some(CommandResult {
                    response: format!("Think mode set to: {level}"),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: true,
                })
            } else {
                Some(CommandResult {
                    response: "Session not found".into(),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                })
            }
        }

        "/react" => {
            if arg.is_empty() {
                let sessions = state.sessions.lock().await;
                let on = sessions
                    .get(current_session_id)
                    .map(|s| s.show_react)
                    .unwrap_or_else(default_show_react);
                return Some(CommandResult {
                    response: format!(
                        "react: {}\nUsage: /react <on|off>",
                        if on { "on" } else { "off" }
                    ),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let val = arg.to_lowercase();
            let on = match val.as_str() {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                _ => {
                    return Some(CommandResult {
                        response: format!("Invalid value: {arg}\nUsage: /react <on|off>"),
                        response_type: "system",
                        new_session_id: None,
                        sessions_changed: false,
                    });
                }
            };
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                session.show_react = on;
                Some(CommandResult {
                    response: format!("React visibility: {}", if on { "on" } else { "off" }),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: true,
                })
            } else {
                Some(CommandResult {
                    response: "Session not found".into(),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                })
            }
        }

        "/tool" => {
            if arg.is_empty() {
                let sessions = state.sessions.lock().await;
                let on = sessions
                    .get(current_session_id)
                    .map(|s| s.show_tools)
                    .unwrap_or_else(default_show_tools);
                return Some(CommandResult {
                    response: format!(
                        "tool: {}\nUsage: /tool <on|off>",
                        if on { "on" } else { "off" }
                    ),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let val = arg.to_lowercase();
            let on = match val.as_str() {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                _ => {
                    return Some(CommandResult {
                        response: format!("Invalid value: {arg}\nUsage: /tool <on|off>"),
                        response_type: "system",
                        new_session_id: None,
                        sessions_changed: false,
                    });
                }
            };
            match persist_session_toggle(state, current_session_id, |session| {
                session.show_tools = on;
            })
            .await
            {
                Ok(()) => Some(CommandResult {
                    response: format!("Tool visibility: {}", if on { "on" } else { "off" }),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                }),
                Err(err) if err == "Session not found" => Some(CommandResult {
                    response: err,
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                }),
                Err(err) => Some(CommandResult {
                    response: format!("Failed to persist tool visibility: {err}"),
                    response_type: "error",
                    new_session_id: None,
                    sessions_changed: false,
                }),
            }
        }

        "/reasoning" => {
            if arg.is_empty() {
                let sessions = state.sessions.lock().await;
                let on = sessions
                    .get(current_session_id)
                    .map(|s| s.show_reasoning)
                    .unwrap_or_else(default_show_reasoning);
                return Some(CommandResult {
                    response: format!(
                        "reasoning: {}\nUsage: /reasoning <on|off>",
                        if on { "on" } else { "off" }
                    ),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let val = arg.to_lowercase();
            let on = match val.as_str() {
                "on" | "true" | "1" => true,
                "off" | "false" | "0" => false,
                _ => {
                    return Some(CommandResult {
                        response: format!("Invalid value: {arg}\nUsage: /reasoning <on|off>"),
                        response_type: "system",
                        new_session_id: None,
                        sessions_changed: false,
                    });
                }
            };
            match persist_session_toggle(state, current_session_id, |session| {
                session.show_reasoning = on;
            })
            .await
            {
                Ok(()) => Some(CommandResult {
                    response: format!("Reasoning visibility: {}", if on { "on" } else { "off" }),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                }),
                Err(err) if err == "Session not found" => Some(CommandResult {
                    response: err,
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                }),
                Err(err) => Some(CommandResult {
                    response: format!("Failed to persist reasoning visibility: {err}"),
                    response_type: "error",
                    new_session_id: None,
                    sessions_changed: false,
                }),
            }
        }

        "/help" => {
            let mut help = "\
Commands:
  /new             Compress conversation to memory & clear context
  /status          Show session status
  /model [name]    Show or switch model
  /think [level]   Set thinking mode (auto|off|minimal|low|medium|high|xhigh)
  /react [on|off]  Toggle ReAct phase visibility
  /tool [on|off]   Toggle tool card visibility
  /reasoning [on|off] Toggle reasoning visibility
  /skills          List available skills
  /rename <name>   Rename current session
  /clear           Clear messages (keep system prompt)
  /help            Show this help"
                .to_string();
            if current_session_id == MAIN_SESSION_ID {
                help.push_str(
                    "\n\nMain session commands:\n\
                  /sessions        List all active sessions\n\
                                    /delete <id>     Delete a session by full ID or unique prefix",
                );
            }
            Some(CommandResult {
                response: help,
                response_type: "system",
                new_session_id: None,
                sessions_changed: false,
            })
        }

        "/sessions" => {
            if current_session_id != MAIN_SESSION_ID {
                return Some(CommandResult {
                    response: "This command is only available in the main session.".into(),
                    response_type: "error",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let output = gather_sessions_status(state).await;
            Some(CommandResult {
                response: output,
                response_type: "system",
                new_session_id: None,
                sessions_changed: false,
            })
        }

        "/delete" => {
            if current_session_id != MAIN_SESSION_ID {
                return Some(CommandResult {
                    response: "This command is only available in the main session.".into(),
                    response_type: "error",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            if arg.is_empty() {
                return Some(CommandResult {
                    response: "Usage: /delete <session_id>".into(),
                    response_type: "system",
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let result = delete_session_by_id(arg, state).await;
            let changed = result.starts_with("Deleted");
            Some(CommandResult {
                response: result,
                response_type: "system",
                new_session_id: None,
                sessions_changed: changed,
            })
        }

        _ => None,
    }
}

// ── WebSocket Handler ────────────────────────────────────────────────────────

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
    let (mut socket_tx, mut rx) = socket.split();
    let (tx, mut outbound_rx) = mpsc::channel::<String>(256);
    let connection_id = state.next_connection_id.fetch_add(1, Ordering::Relaxed);
    let connection_cancel = CancellationToken::new();
    let writer_cancel = connection_cancel.clone();
    let writer = tokio::spawn(async move {
        while let Some(msg) = outbound_rx.recv().await {
            if socket_tx.send(WsMsg::Text(msg.into())).await.is_err() {
                writer_cancel.cancel();
                break;
            }
        }
    });
    // Reader task: forward incoming text to a channel and signal disconnect
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<String>(32);
    let reader_cancel = connection_cancel.clone();
    let reader = tokio::spawn(async move {
        while let Some(result) = rx.next().await {
            match result {
                Ok(WsMsg::Text(t)) => {
                    // Use try_send to never block — if the inbound channel is full
                    // (e.g. agent is busy), drop the message but keep reading so we
                    // always detect Close / Error frames promptly.
                    match inbound_tx.try_send(t.to_string()) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Closed(_)) => break,
                        Err(mpsc::error::TrySendError::Full(_)) => {}
                    }
                }
                Ok(WsMsg::Close(_)) | Err(_) => break,
                _ => continue,
            }
        }
        reader_cancel.cancel();
    });

    // Resume a session: prefer the explicitly requested one (browser refresh),
    // then fall back to the most recent unclaimed saved session, then create new.
    let mut current_session_id;

    let mut claimed = if let Some(ref req_id) = requested_id {
        // Client requested a specific session — wait briefly for the old connection to release it
        claim_requested_session(req_id, &state, connection_id).await
    } else {
        None
    };

    // Only fall back to "most recent saved session" when NO specific session was requested.
    // If the client asked for a specific session and it failed, create new (don't hijack another).
    if claimed.is_none() && requested_id.is_none() {
        // Prefer the main session first
        match try_claim_session(MAIN_SESSION_ID, &state, connection_id).await {
            ClaimSessionResult::Claimed(id) => {
                claimed = Some(id);
            }
            ClaimSessionResult::InUse | ClaimSessionResult::NotFound => {
                // Main session in use or gone — fall back to most recent
                let saved_ids: Vec<String> = list_saved_session_summaries()
                    .iter()
                    .filter_map(|s| s["id"].as_str().map(|id| id.to_string()))
                    .collect();

                for cid in &saved_ids {
                    match try_claim_session(cid, &state, connection_id).await {
                        ClaimSessionResult::Claimed(id) => {
                            claimed = Some(id);
                            break;
                        }
                        ClaimSessionResult::InUse | ClaimSessionResult::NotFound => {
                            continue;
                        }
                    }
                }
            }
        }
    }

    if let Some(id) = claimed {
        current_session_id = id.clone();
        let (name, avatar, history, view_state) = {
            let sessions = state.sessions.lock().await;
            // Safe: try_claim_session just inserted this id while holding the lock.
            // Use if-let to satisfy no-unwrap rule, though the None branch is unreachable.
            if let Some(s) = sessions.get(&id) {
                (
                    s.name.clone(),
                    s.avatar.clone(),
                    build_history_payload(s),
                    build_view_state_payload(s),
                )
            } else {
                (
                    "New Chat".into(),
                    None,
                    json!({"type":"history","messages":[]}),
                    json!({"type":"view_state","show_tools":true,"show_reasoning":true,"show_react":true}),
                )
            }
        };
        ws_send(
            &tx,
            &json!({"type":"session","id":&id,"name":&name,"avatar":avatar}),
        )
        .await;
        ws_send(&tx, &view_state).await;
        ws_send(&tx, &history).await;
    } else {
        let mut session = Session::new();
        let sys = build_system_prompt(
            &state.config,
            &session.workspace,
            session.effective_model(&state.config.model),
            false,
        );
        session.messages.push(sys);
        current_session_id = session.id.clone();
        let avatar = session.avatar.clone();
        {
            let mut active = state.active_connections.lock().await;
            let mut sessions = state.sessions.lock().await;
            sessions.insert(current_session_id.clone(), session);
            active.insert(current_session_id.clone(), connection_id);
        }
        ws_send(
            &tx,
            &json!({"type":"session","id":&current_session_id,"name":"New Chat","avatar":avatar}),
        )
        .await;
        if let Some(view_state) = {
            let sessions = state.sessions.lock().await;
            sessions
                .get(&current_session_id)
                .map(build_view_state_payload)
        } {
            ws_send(&tx, &view_state).await;
        }
    }

    bind_session_connection(&state, &current_session_id, connection_id, &tx, false).await;
    replay_live_round(&tx, &state, &current_session_id).await;
    finish_session_replay(&state, &current_session_id, connection_id).await;
    send_sessions_list(&tx, &state, &current_session_id).await;

    let cancel = state.shutdown.clone();
    let current_session_ref = Arc::new(Mutex::new(current_session_id.clone()));
    let (live_tx, mut live_rx) = mpsc::channel::<serde_json::Value>(256);
    let live_state = state.clone();
    let live_session_ref = current_session_ref.clone();
    let live_dispatcher = tokio::spawn(async move {
        while let Some(event) = live_rx.recv().await {
            let session_id = {
                let guard = live_session_ref.lock().await;
                guard.clone()
            };
            dispatch_live_event(&live_state, &session_id, event).await;
        }
    });

    let disconnect_state = state.clone();
    let disconnect_session_ref = current_session_ref.clone();
    let disconnect_cancel = connection_cancel.clone();
    let disconnect_watcher = tokio::spawn(async move {
        disconnect_cancel.cancelled().await;
        let session_id = {
            let guard = disconnect_session_ref.lock().await;
            guard.clone()
        };
        unbind_session_connection_if_matches(&disconnect_state, &session_id, connection_id).await;
    });

    let poll_cancel = connection_cancel.clone();
    let poll_state = state.clone();
    let poll_tx = tx.clone();
    let poll_session_ref = current_session_ref.clone();
    let avatar_poller = tokio::spawn(async move {
        let mut avatar_poll = tokio::time::interval(Duration::from_secs(1));
        avatar_poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = poll_cancel.cancelled() => break,
                _ = avatar_poll.tick() => {
                    let session_id = {
                        let guard = poll_session_ref.lock().await;
                        guard.clone()
                    };
                    if let Some(avatar) = detect_session_avatar_update(&session_id, &poll_state).await {
                        if ws_try_send(&poll_tx, &json!({"type":"avatar_update","avatar":avatar,"session_id":&session_id})) {
                            commit_session_avatar(&session_id, avatar, &poll_state).await;
                        }
                    }
                }
            }
        }
    });

    loop {
        let text = tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = connection_cancel.cancelled() => break,
            result = inbound_rx.recv() => match result {
                Some(text) => text,
                None => break,
            },
        };

        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with('/') {
            let cmd_result = handle_command(
                trimmed,
                &current_session_id,
                connection_id,
                &state,
                &tx,
                &cancel,
            )
            .await;
            if cancel.is_cancelled() {
                break;
            }
            if let Some(result) = cmd_result {
                let refresh_view_state = {
                    let sessions = state.sessions.lock().await;
                    sessions.get(&current_session_id).map(|session| {
                        let view_state = build_view_state_payload(session);
                        let history = if trimmed.starts_with("/tool") {
                            Some(build_history_payload(session))
                        } else {
                            None
                        };
                        (view_state, history)
                    })
                };
                if let Some((view_state, history)) = refresh_view_state {
                    ws_send(&tx, &view_state).await;
                    if let Some(history_payload) = history {
                        ws_send(&tx, &history_payload).await;
                    }
                }

                ws_send(
                    &tx,
                    &json!({"type":result.response_type,"content":result.response}),
                )
                .await;

                if let Some(new_id) = result.new_session_id {
                    unbind_session_connection_if_matches(
                        &state,
                        &current_session_id,
                        connection_id,
                    )
                    .await;
                    state.live_rounds.lock().await.remove(&current_session_id);
                    current_session_id = new_id.clone();
                    bind_session_connection(&state, &current_session_id, connection_id, &tx, true)
                        .await;
                    {
                        let mut active_id = current_session_ref.lock().await;
                        *active_id = current_session_id.clone();
                    }
                    let (name, avatar, view_state) = {
                        let sessions = state.sessions.lock().await;
                        sessions
                            .get(&current_session_id)
                            .map(|s| {
                                (
                                    s.name.clone(),
                                    s.avatar.clone(),
                                    build_view_state_payload(s),
                                )
                            })
                            .unwrap_or_else(|| {
                                (
                                    "New Chat".into(),
                                    None,
                                    json!({"type":"view_state","show_tools":true,"show_reasoning":true,"show_react":true}),
                                )
                            })
                    };
                    ws_send(&tx, &json!({"type":"session_switched","id":&new_id,"name":&name,"avatar":avatar})).await;
                    ws_send(&tx, &view_state).await;
                    let history = {
                        let sessions = state.sessions.lock().await;
                        sessions.get(&current_session_id).map(build_history_payload)
                    };
                    if let Some(payload) = history {
                        ws_send(&tx, &payload).await;
                    }
                }
                if result.sessions_changed {
                    send_sessions_list(&tx, &state, &current_session_id).await;
                }
                if let Some(avatar) =
                    detect_session_avatar_update(&current_session_id, &state).await
                {
                    if ws_send(&tx, &json!({"type":"avatar_update","avatar":avatar,"session_id":&current_session_id})).await {
                        commit_session_avatar(&current_session_id, avatar, &state).await;
                    }
                }
            } else {
                ws_send(
                    &tx,
                    &json!({"type":"system","content":"Unknown command. Type /help."}),
                )
                .await;
            }
            continue;
        }

        {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(&current_session_id) {
                session.messages.push(ChatMessage {
                    role: "user".into(),
                    content: Some(text),
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: Some(now_epoch()),
                });
                session.updated_at = now_epoch();
            }
        }

        let show_react = {
            let sessions = state.sessions.lock().await;
            sessions
                .get(&current_session_id)
                .map(|s| s.show_react)
                .unwrap_or(false)
        };

        let mut shutting_down = false;
        let mut round: usize = 0;
        let mut react_ctx = agent::AgentLoopCtx::new(show_react);
        const AGENT_HARD_CAP_ROUNDS: usize = 200;

        // Inter-phase state: Analyze → Act → Observe
        let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
        let mut collected_results: Vec<agent::ToolResultEntry> = Vec::new();
        let mut cycle_workspace = std::path::PathBuf::new();
        let mut cycle_is_main = false;
        let mut last_observation_hint: Option<String> = None;

        'agent: loop {
            if cancel.is_cancelled() {
                shutting_down = true;
                break;
            }

            match react_ctx.phase() {
                // ── Analyze: snapshot session, call LLM, decide next phase ───
                agent::AgentPhase::Analyze => {
                    if round >= AGENT_HARD_CAP_ROUNDS {
                        let (system_event, done_event) = build_agent_hard_cap_events(
                            AGENT_HARD_CAP_ROUNDS,
                            react_ctx.cycles,
                            react_ctx.tool_calls,
                        );
                        if !live_send(&live_tx, system_event).await {
                            break;
                        }
                        let _ = live_send(&live_tx, done_event).await;
                        break;
                    }

                    let had_observation_hint = last_observation_hint.is_some();

                    let (msgs_snapshot, model, workspace, think_level, prev_avatar, pruned_count) = {
                        let mut sessions = state.sessions.lock().await;
                        let session = match sessions.get_mut(&current_session_id) {
                            Some(s) => s,
                            None => break,
                        };
                        let model_str = session.effective_model(&state.config.model).to_string();
                        let is_main_session = session.is_main();
                        let mut fresh_system = build_system_prompt(
                            &state.config,
                            &session.workspace,
                            &model_str,
                            is_main_session,
                        );
                        // Inject observation context hint from previous cycle
                        if let Some(hint) = last_observation_hint.take() {
                            if let Some(ref mut content) = fresh_system.content {
                                content.push_str("\n\n");
                                content.push_str(&hint);
                            }
                        }
                        if let Some(first) = session.messages.first_mut() {
                            if first.role == "system" {
                                *first = fresh_system;
                            }
                        }
                        let msg_count_before = session.messages.len();
                        prune_messages(
                            &mut session.messages,
                            state.config.context_limit_for_model(&model_str),
                        );
                        let pruned = msg_count_before - session.messages.len();
                        let prev_avatar = session.avatar.clone();
                        (
                            session.messages.clone(),
                            model_str,
                            session.workspace.clone(),
                            session.think_level.clone(),
                            prev_avatar,
                            pruned,
                        )
                    };

                    // Notify frontend when context was pruned
                    if pruned_count > 0 {
                        let _ = live_send(
                            &live_tx,
                            json!({
                                "type": "context_pruned",
                                "messages_removed": pruned_count,
                            }),
                        )
                        .await;
                    }

                    // Stash per-cycle state for Act phase
                    cycle_workspace = workspace.clone();
                    cycle_is_main = current_session_id == MAIN_SESSION_ID;

                    // Parse avatar outside lock (does sync I/O)
                    let avatar = prompts::parse_identity_avatar(&workspace);
                    if avatar != prev_avatar {
                        let mut sessions = state.sessions.lock().await;
                        if let Some(session) = sessions.get_mut(&current_session_id) {
                            session.avatar = avatar.clone();
                        }
                    }

                    if !live_send(
                        &live_tx,
                        json!({
                            "type":"start",
                            "round": round + 1,
                            "avatar": avatar,
                            "phase": react_ctx.phase().label(),
                            "react_visible": react_ctx.show_react,
                        }),
                    )
                    .await
                    {
                        break;
                    }

                    let resolved = state.config.resolve_model(&model);

                    // Phase 3: adapt think level based on cycle depth
                    let effective_think = if think_level == "auto" {
                        if resolved.reasoning || resolved.thinking_format.is_some() {
                            agent::auto_think_level(react_ctx.cycles, had_observation_hint)
                                .to_owned()
                        } else {
                            "off".to_owned()
                        }
                    } else {
                        think_level.clone()
                    };

                    let extra_tools: Vec<serde_json::Value> = if cycle_is_main {
                        match resolved.provider {
                            Provider::Anthropic => admin_tool_definitions_anthropic(),
                            Provider::OpenAI => admin_tool_definitions_openai(),
                        }
                    } else {
                        vec![]
                    };

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
                            &live_tx,
                            &effective_think,
                            &extra_tools,
                        ) => r,
                    };
                    match llm_result {
                        Ok(resp) => {
                            let has_content = resp.message.has_nonempty_content();
                            let has_tools = resp.message.has_tool_calls();
                            let should_persist = !resp.message.is_empty_assistant_message();

                            if should_persist {
                                let mut sessions = state.sessions.lock().await;
                                if let Some(session) = sessions.get_mut(&current_session_id) {
                                    session.messages.push(resp.message.clone());
                                    session.updated_at = now_epoch();
                                }
                            }

                            if let Some(reason) = agent::evaluate_finish(has_content, has_tools) {
                                react_ctx.transition_to_finish(reason);
                                if react_ctx.show_react {
                                    let _ = live_send(
                                        &live_tx,
                                        json!({"type":"react_phase","phase":"finish","cycle":react_ctx.cycles}),
                                    )
                                    .await;
                                }
                            } else {
                                pending_tool_calls = resp.message.tool_calls.unwrap_or_default();
                                react_ctx.transition_to_act();
                                if react_ctx.show_react {
                                    let _ = live_send(
                                        &live_tx,
                                        json!({"type":"react_phase","phase":"act","cycle":react_ctx.cycles}),
                                    )
                                    .await;
                                }
                            }
                            round += 1;
                        }
                        Err(e) => {
                            let _ = live_send(&live_tx, json!({"type":"error","content":e})).await;
                            break;
                        }
                    }
                } // end Analyze

                // ── Act: execute pending tool calls ──────────────────────────
                agent::AgentPhase::Act => {
                    collected_results.clear();

                    for tc in &pending_tool_calls {
                        if cancel.is_cancelled() {
                            shutting_down = true;
                            break 'agent;
                        }

                        if !live_send(
                            &live_tx,
                            json!({
                                "type":"tool_call",
                                "id": tc.id,
                                "name": tc.function.name,
                                "arguments": tc.function.arguments,
                            }),
                        )
                        .await
                        {
                            break 'agent;
                        }

                        let result = if cycle_is_main && is_admin_tool(&tc.function.name) {
                            let start = std::time::Instant::now();
                            let output = execute_admin_tool(
                                &tc.function.name,
                                &tc.function.arguments,
                                &state,
                            )
                            .await;
                            let duration_ms = start.elapsed().as_millis() as u64;
                            let is_error = tools::is_tool_error_output(&tc.function.name, &output);
                            tools::ToolOutcome {
                                output,
                                is_error,
                                duration_ms,
                            }
                        } else {
                            tokio::select! {
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
                                    &cycle_workspace,
                                ) => r,
                            }
                        };

                        if !live_send(
                            &live_tx,
                            json!({
                                "type":"tool_result",
                                "id": tc.id,
                                "name": tc.function.name,
                                "result": result.output,
                                "duration_ms": result.duration_ms,
                                "is_error": result.is_error,
                            }),
                        )
                        .await
                        {
                            break 'agent;
                        }

                        // Collect for Observe phase before persisting
                        collected_results.push(agent::ToolResultEntry {
                            id: tc.id.clone(),
                            name: tc.function.name.clone(),
                            duration_ms: result.duration_ms,
                            is_error: result.is_error,
                            result: result.output.clone(),
                        });

                        {
                            let mut sessions = state.sessions.lock().await;
                            if let Some(session) = sessions.get_mut(&current_session_id) {
                                session.messages.push(ChatMessage {
                                    role: "tool".into(),
                                    content: Some(result.output),
                                    tool_calls: None,
                                    tool_call_id: Some(tc.id.clone()),
                                    timestamp: Some(now_epoch()),
                                });
                                session.tool_calls_count += 1;
                            }
                        }
                    }

                    let tc_count = pending_tool_calls.len();
                    pending_tool_calls.clear();
                    react_ctx.transition_to_observe(tc_count);
                    if react_ctx.show_react {
                        let _ = live_send(
                            &live_tx,
                            json!({"type":"react_phase","phase":"observe","cycle":react_ctx.cycles}),
                        )
                        .await;
                    }
                } // end Act

                // ── Observe: summarize large results, save to disk ───────────
                agent::AgentPhase::Observe => {
                    // Non-destructive observation summaries
                    let summaries = agent::summarize_observations(&collected_results);
                    for s in &summaries {
                        let _ = live_send(
                            &live_tx,
                            json!({
                                "type": "observation",
                                "tool_call_id": s.tool_call_id,
                                "tool_name": s.tool_name,
                                "byte_size": s.byte_size,
                                "line_count": s.line_count,
                                "hint": s.hint,
                            }),
                        )
                        .await;
                    }
                    // Store hint for next Analyze round's system prompt
                    last_observation_hint = agent::build_observation_context_hint(&summaries);
                    collected_results.clear();

                    // Incremental save so progress is not lost on crash.
                    let snapshot = {
                        let sessions = state.sessions.lock().await;
                        sessions.get(&current_session_id).cloned()
                    };
                    if let Some(ref s) = snapshot {
                        let _ = save_session_to_disk(s).await;
                    }

                    react_ctx.transition_to_analyze();
                    if react_ctx.show_react {
                        let _ = live_send(
                            &live_tx,
                            json!({"type":"react_phase","phase":"analyze","cycle":react_ctx.cycles}),
                        )
                        .await;
                    }
                } // end Observe

                // ── Finish: send done, save, break ───────────────────────────
                agent::AgentPhase::Finish => {
                    // Incremental save (also covers no-tool responses)
                    let snapshot = {
                        let sessions = state.sessions.lock().await;
                        sessions.get(&current_session_id).cloned()
                    };
                    if let Some(ref s) = snapshot {
                        let _ = save_session_to_disk(s).await;
                    }

                    let finish_label = react_ctx
                        .finish_reason
                        .map(|r| r.label())
                        .unwrap_or("complete");

                    let _ = live_send(
                        &live_tx,
                        json!({
                            "type":"done",
                            "phase":"finish",
                            "reason": finish_label,
                            "cycles": react_ctx.cycles,
                            "tool_calls": react_ctx.tool_calls,
                        }),
                    )
                    .await;
                    break;
                } // end Finish
            } // end match
        } // end 'agent loop

        if shutting_down {
            let _ = live_send(
                &live_tx,
                json!({"type":"system","content":"Server shutting down."}),
            )
            .await;
        }

        if shutting_down {
            break;
        }
    }

    connection_cancel.cancel();

    // Trim incomplete tool-call transactions and save once on disconnect.
    {
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
        match save_session_to_disk(s).await {
            Ok(()) => {
                let has_active_connection = state
                    .active_connections
                    .lock()
                    .await
                    .contains_key(&current_session_id);
                if !has_active_connection {
                    let mut sessions = state.sessions.lock().await;
                    sessions.remove(&current_session_id);
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: failed to save session {} on disconnect: {e}; keeping in memory",
                    s.id
                );
            }
        }
    } else {
        let has_active_connection = state
            .active_connections
            .lock()
            .await
            .contains_key(&current_session_id);
        if !has_active_connection {
            let mut sessions = state.sessions.lock().await;
            sessions.remove(&current_session_id);
        }
    }
    unbind_session_connection_if_matches(&state, &current_session_id, connection_id).await;
    state.live_rounds.lock().await.remove(&current_session_id);
    drop(tx);
    drop(live_tx);
    let _ = disconnect_watcher.await;
    let _ = live_dispatcher.await;
    let _ = avatar_poller.await;
    let _ = reader.await;
    let _ = writer.await;
}

/// Wait for a specific session to become available (old connection releasing it),
/// then load from disk and claim it. Returns None if unavailable after timeout.
async fn claim_requested_session(id: &str, state: &AppState, connection_id: u64) -> Option<String> {
    // Validate ID format
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return None;
    }
    // Wait up to 30 seconds for an in-flight generation round to finish and release the session.
    // If the old connection still has an active client binding (socket not disconnected),
    // the session is genuinely in use — bail out immediately instead of blocking.
    for _ in 0..60 {
        let active = state.active_connections.lock().await.contains_key(id);
        if !active {
            break;
        }
        let has_bound_client = state.session_clients.lock().await.contains_key(id);
        if has_bound_client {
            // Old connection is still alive and bound — not a refresh scenario.
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    match try_claim_session(id, state, connection_id).await {
        ClaimSessionResult::Claimed(id) => Some(id),
        ClaimSessionResult::InUse | ClaimSessionResult::NotFound => None,
    }
}

async fn send_sessions_list(tx: &WsTx, state: &AppState, active_id: &str) {
    // Merge in-memory sessions with on-disk summaries
    let in_mem: HashMap<String, serde_json::Value> = {
        let sessions = state.sessions.lock().await;
        sessions
            .iter()
            .map(|(id, s)| {
                let msg_count = s.messages.iter().filter(|m| m.role != "system").count();
                (
                    id.clone(),
                    json!({
                        "id": id, "name": s.name, "messages": msg_count,
                        "created_at": s.created_at, "updated_at": s.updated_at,
                        "active": id == active_id,
                    }),
                )
            })
            .collect()
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
        let b_ts = b["updated_at"].as_u64().unwrap_or(0);
        let a_ts = a["updated_at"].as_u64().unwrap_or(0);
        b_ts.cmp(&a_ts)
    });
    // Filter out empty sessions (0 user messages) from the sidebar — they're just
    // reconnect placeholders on disk, not useful for the user to see.
    all.retain(|s| {
        s["active"].as_bool() == Some(true)
            || s["messages"].as_u64().unwrap_or(0) > 0
            || s["corrupt"].as_bool() == Some(true)
    });
    ws_send(tx, &json!({"type":"sessions_list","sessions":all})).await;
}

// ── HTTP API ──────────────────────────────────────────────────────────────────

async fn api_shutdown(headers: HeaderMap, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    // Verify shutdown token — only the local CLI should be able to trigger this
    let provided = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if provided != state.shutdown_token {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        );
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

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Parse --port <N> from anywhere in args
    let port_override: Option<u16> = args
        .windows(2)
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
        eprintln!(
            "  Config providers: {} ({} models)",
            names.join(", "),
            total
        );
    }
    eprintln!("  Exec timeout:  {}s", config.exec_timeout.as_secs());
    eprintln!(
        "  Context limit: {} tokens",
        config.context_limit_for_model(&config.model)
    );

    let shutdown = CancellationToken::new();

    // Generate a one-time shutdown token and write it to disk for CLI use
    let shutdown_token = generate_shutdown_token();
    if let Some(dir) = config_dir_path() {
        let _ = std::fs::write(dir.join(format!("shutdown-{port}.token")), &shutdown_token);
    }

    let state = Arc::new(AppState {
        config,
        http: Client::new(),
        sessions: Mutex::new(HashMap::new()),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: shutdown.clone(),
        shutdown_token,
    });

    // Ensure main session exists (load from disk or create fresh)
    {
        let main_session = load_session_from_disk(MAIN_SESSION_ID).unwrap_or_else(|| {
            let mut s = Session::new_with_id(MAIN_SESSION_ID, "Main");
            let model = s.effective_model(&state.config.model).to_string();
            let sys = build_system_prompt(&state.config, &s.workspace, &model, true);
            s.messages.push(sys);
            s
        });
        state
            .sessions
            .lock()
            .await
            .insert(MAIN_SESSION_ID.to_string(), main_session);
        eprintln!("  Main session: ready");
    }

    let static_dir = resolve_static_dir();
    eprintln!("  Static dir:    {}", static_dir.display());

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/health", get(api_health))
        .route("/api/sessions", get(api_sessions))
        .route("/api/shutdown", post(api_shutdown))
        .fallback_service(ServeDir::new(static_dir).append_index_html_on_directories(true))
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

#[cfg(test)]
mod main_tests;
