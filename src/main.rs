use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Multipart, Query, Request, State,
        ws::{Message as WsMsg, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, mpsc, mpsc::error::TrySendError};
use tokio_util::sync::CancellationToken;
use tower_http::services::ServeDir;

mod agent;
mod cli;
mod commands;
mod config;
mod context;
mod hooks;
mod image_uploads;
mod memory;
mod plan;
mod prompts;
mod providers;
mod runtime_loop;
mod session_admin;
mod session_control;
mod session_group;
mod session_store;
mod socket_sync;
mod socket_tasks;
mod storage;
mod subagents;
mod todos;
mod tools;

pub(crate) use config::{Config, DEFAULT_PORT, Provider, config_dir_path, config_file_path};
pub(crate) use context::{
    accumulate_daily_token_usage, context_input_budget_for_model, current_daily_token_usage,
    estimate_tokens_for_provider, format_token_count, format_usage_block,
    message_token_len_for_provider, split_usage_labels, update_session_token_usage_with_provider,
    update_session_token_usage_with_providers,
};
pub(crate) use hooks::{
    AutoCompressContextHook, CommandHookInput, HookRegistry, LlmHookInput, ToolHookInput,
    run_command_hooks, run_hooks, run_llm_hooks, run_tool_hooks,
};
pub(crate) use memory::MemoryUpdateQueue;

use commands::handle_command;
use runtime_loop::{
    IdleSocketInputAction, ensure_session_ready, handle_idle_socket_input,
    resolve_or_create_socket_session, run_agent_session,
};
#[cfg(test)]
use session_store::load_session_from_disk;
use session_store::{
    SessionSummary, refresh_session_system_prompt, save_session_to_disk_locked,
    session_persist_gate, sessions_dir,
};
use socket_sync::{
    broadcast_session_list_payload, build_session_info_payload, send_command_refresh,
    send_existing_session_payloads,
};
use socket_tasks::{ConnectionCleanup, finalize_connection, spawn_connection_tasks};

#[cfg(test)]
use config::{JsonConfig, JsonModelEntry, JsonProviderConfig};
#[cfg(test)]
use context::{
    estimate_request_tokens_for_provider, estimate_tokens, message_token_len, prune_messages,
    turn_len,
};
#[cfg(test)]
use hooks::{
    build_auto_summary_message, build_compressed_messages, build_compression_source_text,
    find_auto_compress_cutoff,
};
#[cfg(test)]
use session_admin::gather_global_today_usage;
#[cfg(test)]
use session_store::list_saved_session_summaries_in_dir;
#[cfg(test)]
use session_store::save_session_to_disk;
#[cfg(test)]
use session_store::{
    build_active_session_lines, build_global_today_usage, build_history_payload,
    build_session_status, build_session_usage, build_usage_report, list_saved_session_ids_in_dir,
    recoverable_session_ids_from_summaries, replace_session_messages, resolve_session_target,
    sanitize_session_messages, subagent_snapshot_storage_key, trim_incomplete_tool_calls,
    trim_incomplete_tool_calls_in_session,
};
use std::collections::{HashSet, VecDeque};

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const MAIN_SESSION_ID: &str = "main";

/// True when `id` denotes the main session, case-insensitively. Main protection
/// must be uniform across OSes, so this never depends on `cfg!(windows)`.
pub(crate) fn is_main(id: &str) -> bool {
    id.eq_ignore_ascii_case(MAIN_SESSION_ID)
}

/// Whether two session ids refer to the same session under the existing policy:
/// exact match always, or case-insensitive only on Windows (case-insensitive FS).
pub(crate) fn session_ids_match(a: &str, b: &str) -> bool {
    a == b || (cfg!(windows) && a.eq_ignore_ascii_case(b))
}

const INBOUND_BUFFER_CAPACITY: usize = 128;
static CONFIG_FILE_LOCK: std::sync::LazyLock<tokio::sync::RwLock<()>> =
    std::sync::LazyLock::new(|| tokio::sync::RwLock::new(()));
static S3_LIFECYCLE_SYNC_LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));
static S3_LIFECYCLE_SYNCED_CONFIG_ID: std::sync::LazyLock<std::sync::Mutex<Option<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));
/// Process-local, monotonically increasing runtime config revision. The value is
/// paired with `AppState.config` while holding the same mutex so payload builders
/// can never combine a Config snapshot with a revision from another update.
static CONFIG_REVISION: std::sync::LazyLock<AtomicU64> = std::sync::LazyLock::new(|| {
    let epoch_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    AtomicU64::new(epoch_millis.max(1))
});
#[cfg(test)]
static CONFIG_REVISION_BUMPS_BY_STATE: std::sync::LazyLock<std::sync::Mutex<HashMap<usize, u64>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

// ── Data Models ──────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug)]
pub(crate) struct ImageAttachment {
    pub(crate) url: String,
    /// Human-readable display name. Older persisted attachments omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// Validated media type for tool images. Older user attachments may omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mime_type: Option<String>,
    /// Persisted S3 object key for locally uploaded images so fresh
    /// presigned URLs can be generated for history replay and provider calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) s3_object_key: Option<String>,
    /// Opaque identity of the S3 configuration that accepted this upload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) s3_config_id: Option<String>,
    /// Persisted path to a cached base64 file inside the session workspace.
    /// This keeps historical Ollama images available across restarts without
    /// bloating the session JSON itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cache_path: Option<String>,
    /// Cached base64-encoded image data.  Populated at intake so Ollama
    /// requests never re-fetch historical URLs.  Not persisted to disk
    /// (`skip_serializing`) to avoid bloating session files; after a reload
    /// the disk cache or legacy network fallback in `fetch_images_base64`
    /// handles it.
    #[serde(skip_serializing, default)]
    pub(crate) data: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct AnthropicThinkingBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<String>,
}

pub(crate) const OPENAI_RESPONSES_RESPONSE_ID_BLOCK_TYPE: &str = "openai_responses_response_id";

pub(crate) fn is_visible_anthropic_thinking_block(block: &AnthropicThinkingBlock) -> bool {
    block.block_type != OPENAI_RESPONSES_RESPONSE_ID_BLOCK_TYPE
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub(crate) struct SubagentToolHistorySnapshot {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageAttachment>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
pub(crate) struct SubagentHistorySnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<SubagentToolHistorySnapshot>,
    #[serde(default)]
    pub cycles: usize,
    #[serde(default)]
    pub tool_calls: usize,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    images: Option<Vec<ImageAttachment>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    anthropic_thinking_blocks: Option<Vec<AnthropicThinkingBlock>>,
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

    fn has_anthropic_thinking_blocks(&self) -> bool {
        self.anthropic_thinking_blocks
            .as_ref()
            .is_some_and(|blocks| blocks.iter().any(is_visible_anthropic_thinking_block))
    }

    fn has_nonempty_thinking(&self) -> bool {
        self.thinking
            .as_deref()
            .is_some_and(|thinking| !thinking.is_empty())
    }

    fn is_empty_assistant_message(&self) -> bool {
        self.role == "assistant"
            && !self.has_nonempty_content()
            && !self.has_nonempty_thinking()
            && !self.has_tool_calls()
            && !self.has_anthropic_thinking_blocks()
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gemini_thought_signature: Option<String>,
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

const SESSION_VERSION: u32 = 7;
pub(crate) use plan::PendingPlan;

#[derive(Clone, Serialize, Deserialize)]
struct Session {
    id: String,
    name: String,
    messages: Vec<ChatMessage>,
    created_at: u64,
    updated_at: u64,
    tool_calls_count: usize,
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    daily_input_tokens: u64,
    #[serde(default)]
    daily_output_tokens: u64,
    #[serde(default = "default_token_usage_source")]
    input_token_source: String,
    #[serde(default = "default_token_usage_source")]
    output_token_source: String,
    #[serde(default)]
    token_usage_day: String,
    /// Per-day usage labels (provider:* / role:*) reset together with daily totals.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    daily_provider_usage: HashMap<String, [u64; 2]>,
    /// Lifetime usage labels (provider:* / role:*), never reset unless the session is deleted.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    total_label_usage: HashMap<String, [u64; 2]>,
    /// Historical daily usage snapshots (capped at USAGE_HISTORY_CAP days).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    usage_history: Vec<DailyUsageSnapshot>,
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
    /// System skill paths enabled for this session (e.g. "anthropics", "anthropics/pdf").
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    enabled_system_skills: HashSet<String>,
    /// Legacy field read from older session files. System skill injection is now
    /// allow-list based through `enabled_system_skills`.
    #[serde(default, skip_serializing)]
    #[allow(dead_code)]
    disabled_system_skills: HashSet<String>,
    /// Tool call ids whose persisted tool result ended in an error state.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    failed_tool_results: HashSet<String>,
    /// Compact delegated-task snapshots keyed by parent `task` tool_call_id.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    subagent_snapshots: HashMap<String, SubagentHistorySnapshot>,
    #[serde(default)]
    todos: todos::TodoSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending_plan: Option<PendingPlan>,
    #[serde(default)]
    version: u32,
    #[serde(skip)]
    workspace: PathBuf,
}

/// One day's aggregated token usage (stored in `usage_history`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct DailyUsageSnapshot {
    #[serde(default)]
    pub(crate) date: String,
    #[serde(default)]
    pub(crate) input: u64,
    #[serde(default)]
    pub(crate) output: u64,
    /// Per-day usage labels (legacy raw provider names or provider:* / role:*).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub(crate) providers: HashMap<String, [u64; 2]>,
}

/// Maximum number of daily snapshots kept in usage_history.
const USAGE_HISTORY_CAP: usize = 30;

fn default_think_level() -> String {
    "auto".to_string()
}

fn default_token_usage_source() -> String {
    "estimated".to_string()
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
    if session.version < 4 {
        session.input_token_source = default_token_usage_source();
        session.output_token_source = default_token_usage_source();
    }
    if session.version < 5 {
        session.todos = todos::TodoSnapshot::empty(session.updated_at);
    }
    if session.version < 6 {
        session.enabled_system_skills = HashSet::new();
    }
    if session.version < 7 {
        session.pending_plan = None;
    }
    if let Some(plan) = session.pending_plan.as_mut() {
        plan.normalize_legacy(&session.messages);
    }
    todos::normalize_snapshot(&mut session.todos, session.updated_at);
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
    fn new_with_id(id: &str, name: &str) -> Self {
        let workspace = session_workspace_path(id);
        std::fs::create_dir_all(&workspace).ok();
        prompts::init_session_prompt_files(&workspace);
        Self {
            id: id.to_string(),
            name: name.to_string(),
            messages: Vec::new(),
            created_at: now_epoch(),
            updated_at: now_epoch(),
            tool_calls_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            daily_input_tokens: 0,
            daily_output_tokens: 0,
            input_token_source: default_token_usage_source(),
            output_token_source: default_token_usage_source(),
            token_usage_day: prompts::current_local_snapshot().today(),
            daily_provider_usage: HashMap::new(),
            total_label_usage: HashMap::new(),
            usage_history: Vec::new(),
            model_override: None,
            think_level: default_think_level(),
            show_react: default_show_react(),
            show_tools: default_show_tools(),
            show_reasoning: default_show_reasoning(),
            enabled_system_skills: HashSet::new(),
            disabled_system_skills: HashSet::new(),
            failed_tool_results: HashSet::new(),
            subagent_snapshots: HashMap::new(),
            todos: todos::TodoSnapshot::empty(now_epoch()),
            pending_plan: None,
            version: SESSION_VERSION,
            workspace,
        }
    }

    fn effective_model<'a>(&'a self, default: &'a str) -> &'a str {
        self.model_override.as_deref().unwrap_or(default)
    }

    fn validated_model_override(&self, config: &Config) -> Option<String> {
        self.model_override
            .as_deref()
            .and_then(|model| config.selectable_model_ref(model).ok())
    }

    fn has_explicit_model_override(&self, config: &Config) -> bool {
        self.validated_model_override(config).is_some()
    }

    fn model_configuration(&self, config: &Config) -> (String, bool, bool) {
        if self.model_override.is_some() {
            match self.validated_model_override(config) {
                Some(model) => (model, true, true),
                None => (
                    self.model_override.clone().unwrap_or_default(),
                    false,
                    false,
                ),
            }
        } else {
            (
                config.model.clone(),
                false,
                config.explicit_primary_model_configured,
            )
        }
    }

    fn effective_model_configured(&self, config: &Config) -> bool {
        self.model_configuration(config).2
    }
}

const UPLOAD_TOKEN_HEADER: &str = "x-lingclaw-upload-token";

#[derive(Clone)]
pub(crate) struct SessionModelSnapshot {
    pub(crate) config: Arc<Config>,
    pub(crate) model: String,
    pub(crate) explicit: bool,
}

async fn session_model_snapshot(
    state: &AppState,
    session_id: &str,
) -> Option<SessionModelSnapshot> {
    let _model_status_guard = CONFIG_FILE_LOCK.read().await;
    let config = state.config();
    let sessions = state.sessions.lock().await;
    let session = sessions.get(session_id)?;
    let (model, _, explicit) = session.model_configuration(&config);
    Some(SessionModelSnapshot {
        model,
        explicit,
        config,
    })
}

async fn session_has_explicit_model(state: &AppState, session_id: &str) -> bool {
    session_model_snapshot(state, session_id)
        .await
        .is_some_and(|snapshot| snapshot.explicit)
}

struct AppState {
    #[cfg(not(test))]
    storage: storage::Database,
    config: std::sync::Mutex<Arc<Config>>,
    http: Client,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    /// Session IDs with the connection currently attached to live streaming output.
    active_connections: Mutex<HashMap<String, u64>>,
    session_clients: Mutex<HashMap<String, SessionClientBinding>>,
    group_clients: Mutex<HashMap<String, GroupClientBinding>>,
    live_rounds: Mutex<HashMap<String, LiveRoundState>>,
    /// Per-session active agent runs keyed by the owning connection.
    active_runs: Mutex<HashMap<String, SessionRunBinding>>,
    /// Per-session connection-level cancellation tokens (kick old connection on rebind).
    connection_cancels: Mutex<HashMap<String, ConnectionCancelBinding>>,
    session_control_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    next_connection_id: AtomicU64,
    shutdown: CancellationToken,
    shutdown_token: String,
    upload_token: String,
    hooks: HookRegistry,
    /// Background structured memory updater (active when config.structured_memory is true).
    memory_queue: std::sync::Mutex<Option<MemoryUpdateQueue>>,
}

impl AppState {
    fn storage_status(&self) -> storage::StorageStatus {
        #[cfg(not(test))]
        {
            self.storage.status()
        }
        #[cfg(test)]
        {
            storage::StorageStatus::default()
        }
    }

    fn storage_is_writable(&self) -> bool {
        #[cfg(not(test))]
        {
            self.storage.is_writable()
        }
        #[cfg(test)]
        {
            true
        }
    }

    fn next_config_revision(&self) -> u64 {
        let revision = CONFIG_REVISION.fetch_add(1, Ordering::AcqRel) + 1;
        #[cfg(test)]
        {
            let state_key = self as *const Self as usize;
            let mut counts = CONFIG_REVISION_BUMPS_BY_STATE
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *counts.entry(state_key).or_default() += 1;
        }
        revision
    }

    #[cfg(test)]
    fn config_revision_bump_count_for_test(&self) -> u64 {
        let state_key = self as *const Self as usize;
        CONFIG_REVISION_BUMPS_BY_STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&state_key)
            .copied()
            .unwrap_or_default()
    }

    /// Return a snapshot of the current runtime config.
    fn config(&self) -> Arc<Config> {
        self.config_snapshot_with_revision().0
    }

    /// Return one atomic runtime config/revision pair.
    fn config_snapshot_with_revision(&self) -> (Arc<Config>, u64) {
        match self.config.lock() {
            Ok(guard) => (guard.clone(), CONFIG_REVISION.load(Ordering::Acquire)),
            Err(poisoned) => {
                eprintln!("Warning: config lock poisoned; recovering with inner value");
                (
                    poisoned.into_inner().clone(),
                    CONFIG_REVISION.load(Ordering::Acquire),
                )
            }
        }
    }

    /// Advance the model-configuration revision without replacing the global
    /// Config, for example after a persisted Session `/model` override changes.
    fn bump_config_revision(&self) -> (Arc<Config>, u64) {
        match self.config.lock() {
            Ok(guard) => {
                let revision = self.next_config_revision();
                (guard.clone(), revision)
            }
            Err(poisoned) => {
                eprintln!("Warning: config lock poisoned during revision bump; recovering");
                let guard = poisoned.into_inner();
                let revision = self.next_config_revision();
                (guard.clone(), revision)
            }
        }
    }

    /// Hot-swap the runtime config (called after saving to disk).
    fn replace_config(&self, new: Config) -> (Arc<Config>, u64) {
        let new = Arc::new(new);
        match self.config.lock() {
            Ok(mut guard) => {
                *guard = new.clone();
                let revision = self.next_config_revision();
                (new, revision)
            }
            Err(poisoned) => {
                eprintln!("Warning: config lock poisoned during replace; recovering");
                let mut guard = poisoned.into_inner();
                *guard = new.clone();
                let revision = self.next_config_revision();
                (new, revision)
            }
        }
    }

    fn apply_runtime_config(&self, new: Config) -> (Arc<Config>, u64) {
        self.sync_memory_queue(&new);
        runtime_loop::refresh_reflection_runtime(new.daily_reflection);
        if !new.daily_reflection {
            runtime_loop::cancel_active_reflections();
        }
        self.replace_config(new)
    }

    fn memory_queue(&self) -> Option<MemoryUpdateQueue> {
        match self.memory_queue.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => {
                eprintln!("Warning: memory queue lock poisoned; recovering with inner value");
                poisoned.into_inner().clone()
            }
        }
    }

    fn sync_memory_queue(&self, config: &Config) {
        let sessions = self.sessions.clone();
        let apply = |guard: &mut Option<MemoryUpdateQueue>| match (
            guard.as_ref(),
            config.structured_memory,
        ) {
            (Some(queue), true) => queue.replace_config(config.clone()),
            (None, true) => {
                *guard = Some(MemoryUpdateQueue::spawn(config.clone(), sessions.clone()));
            }
            (_, false) => {
                if let Some(queue) = guard.as_ref() {
                    queue.shutdown();
                }
                *guard = None;
            }
        };
        match self.memory_queue.lock() {
            Ok(mut guard) => apply(&mut guard),
            Err(poisoned) => {
                eprintln!("Warning: memory queue lock poisoned during sync; recovering");
                apply(&mut poisoned.into_inner());
            }
        }
    }
}

#[derive(Clone)]
struct SessionClientBinding {
    connection_id: u64,
    tx: WsTx,
    replay_ready: bool,
    pending_events: VecDeque<serde_json::Value>,
    live_send_in_progress: bool,
}

#[derive(Clone)]
struct GroupClientBinding {
    connection_id: u64,
    tx: WsTx,
    cancel: CancellationToken,
}

#[derive(Clone)]
struct SessionRunBinding {
    connection_id: u64,
    cancel: CancellationToken,
    stop_requested: Arc<AtomicBool>,
    deferred_interventions: Arc<Mutex<DeferredInterventionState>>,
}

#[derive(Default)]
pub(crate) struct DeferredInterventionState {
    queue: Vec<String>,
    accepting: bool,
}

impl DeferredInterventionState {
    fn open() -> Self {
        Self {
            queue: Vec::new(),
            accepting: true,
        }
    }
}

#[derive(Clone)]
struct ConnectionCancelBinding {
    connection_id: u64,
    cancel: CancellationToken,
}

#[derive(Clone, Default)]
struct LiveToolState {
    id: String,
    name: String,
    arguments: String,
    live_output: String,
    result: Option<String>,
    elapsed_ms: u64,
    images: Vec<serde_json::Value>,
    is_error: bool,
}

#[derive(Clone, Default)]
struct LiveCompressionState {
    outcome: Option<String>,
    reason: Option<String>,
    messages_removed: Option<usize>,
    before_estimate: Option<usize>,
    after_estimate: Option<usize>,
    saved_tokens: Option<usize>,
    saved_percent: Option<usize>,
    pruned_messages_removed: Option<usize>,
}

#[derive(Clone, Default)]
struct LiveRoundState {
    connection_id: u64,
    round: usize,
    react_visible: bool,
    phase: Option<String>,
    cycle: Option<usize>,
    effective_model: Option<String>,
    effective_think: Option<String>,
    run_mode: Option<String>,
    auto_observation_strength: Option<String>,
    auto_stagnation_streak: Option<usize>,
    auto_error_streak: Option<usize>,
    auto_task_pressure: Option<usize>,
    auto_action_oriented: Option<bool>,
    auto_ready_to_finish: Option<bool>,
    auto_has_blocking_uncertainty: Option<bool>,
    latest_auto_trace: Option<agent::AutoThinkTrace>,
    latest_task_plan: Option<serde_json::Value>,
    latest_compression: LiveCompressionState,
    has_pending_pre_start_context_updates: bool,
    has_observation: bool,
    assistant_text: String,
    reasoning_text: String,
    reasoning_done: bool,
    tools: Vec<LiveToolState>,
    /// Ordered delegated-task/orchestration events for reconnect replay.
    delegated_events: Vec<serde_json::Value>,
    /// Currently active delegated tasks keyed by stable replay identifier.
    active_tasks: HashSet<String>,
    /// Active orchestrations keyed by `orchestrate_id`.
    active_orchestrations: HashSet<String>,
}

fn live_task_key_from_event(event: &serde_json::Value) -> Option<String> {
    if let Some(task_id) = event["task_id"].as_str().filter(|value| !value.is_empty()) {
        return Some(task_id.to_string());
    }

    let orchestrate_id = event["orchestrate_id"]
        .as_str()
        .filter(|value| !value.is_empty());
    let task_id = event["id"].as_str().filter(|value| !value.is_empty());
    if let (Some(orchestrate_id), Some(task_id)) = (orchestrate_id, task_id) {
        return Some(format!("{orchestrate_id}:{task_id}"));
    }

    event["agent"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn is_subagent_live_event(event: &serde_json::Value) -> bool {
    event["subagent"]
        .as_str()
        .is_some_and(|value| !value.is_empty())
}

fn truncated_live_tool_result_event(event: &serde_json::Value) -> serde_json::Value {
    let mut truncated = event.clone();
    if let Some(obj) = truncated.as_object_mut()
        && let Some(result) = obj.get_mut("result")
        && let Some(result_text) = result.as_str()
    {
        let mut capped = result_text.to_string();
        truncate_safe(&mut capped, LIVE_REPLAY_CAP);
        *result = serde_json::Value::String(capped);
    }
    truncated
}

fn merge_live_tool_output(current: &mut String, stream: Option<&str>, chunk: &str) {
    if chunk.is_empty() {
        return;
    }

    if stream == Some("stderr") {
        current.push_str("\n[stderr]\n");
    }
    current.push_str(chunk);
    truncate_keep_tail_safe(current, LIVE_REPLAY_CAP, "[live output truncated]\n");
}

fn synthetic_tool_call_event_for_output(event: &serde_json::Value) -> Option<serde_json::Value> {
    let tool_id = event["id"].as_str().filter(|value| !value.is_empty())?;
    let tool_name = event["name"].as_str().unwrap_or_default();
    Some(json!({
        "type": "tool_call",
        "id": tool_id,
        "name": tool_name,
        "arguments": "",
        "synthetic": true,
    }))
}

fn synthetic_task_started_event_for_output(event: &serde_json::Value) -> Option<serde_json::Value> {
    let task_id = event["task_id"]
        .as_str()
        .filter(|value| !value.is_empty())?;
    let agent = event["subagent"]
        .as_str()
        .filter(|value| !value.is_empty())?;
    if let Some((orchestrate_id, orchestrate_task_id)) = task_id.split_once(':')
        && !orchestrate_id.is_empty()
        && !orchestrate_task_id.is_empty()
    {
        return Some(json!({
            "type": "orchestrate_task_started",
            "orchestrate_id": orchestrate_id,
            "id": orchestrate_task_id,
            "agent": agent,
            "prompt": "",
        }));
    }
    Some(json!({
        "type": "task_started",
        "task_id": task_id,
        "agent": agent,
        "prompt": "",
    }))
}

fn synthetic_orchestrate_started_event_for_output(
    event: &serde_json::Value,
    delegated_events: &[serde_json::Value],
    existing_tasks: Option<&HashSet<String>>,
) -> Option<serde_json::Value> {
    let task_id = event["task_id"]
        .as_str()
        .filter(|value| !value.is_empty())?;
    let (orchestrate_id, orchestrate_task_id) = task_id.split_once(':')?;
    if orchestrate_id.is_empty() || orchestrate_task_id.is_empty() {
        return None;
    }
    let agent = event["subagent"]
        .as_str()
        .filter(|value| !value.is_empty())?;

    let mut tasks = existing_tasks
        .into_iter()
        .flat_map(|items| items.iter())
        .filter_map(|task_key| {
            let (existing_orchestrate_id, existing_task_id) = task_key.split_once(':')?;
            if existing_orchestrate_id != orchestrate_id || existing_task_id.is_empty() {
                return None;
            }
            let existing_agent = delegated_events
                .iter()
                .rev()
                .find(|event| {
                    (event["type"] == "task_started" || event["type"] == "orchestrate_task_started")
                        && live_task_key_from_event(event).as_deref() == Some(task_key)
                })
                .and_then(|event| event["agent"].as_str())
                .or_else(|| {
                    delegated_events
                        .iter()
                        .rev()
                        .find(|event| {
                            event["type"] == "tool_output"
                                && live_task_key_from_event(event).as_deref() == Some(task_key)
                        })
                        .and_then(|event| event["subagent"].as_str())
                })
                .unwrap_or(agent);
            Some(json!({
                "id": existing_task_id,
                "agent": existing_agent,
                "depends_on": [],
                "prompt_preview": "",
            }))
        })
        .collect::<Vec<_>>();

    if !tasks.iter().any(|task| task["id"] == orchestrate_task_id) {
        tasks.push(json!({
            "id": orchestrate_task_id,
            "agent": agent,
            "depends_on": [],
            "prompt_preview": "",
        }));
    }

    Some(json!({
        "type": "orchestrate_started",
        "orchestrate_id": orchestrate_id,
        "task_count": tasks.len(),
        "layer_count": 1,
        "tasks": tasks,
        "synthetic": true,
    }))
}

/// Truncate `s` in place at the last valid UTF-8 char boundary ≤ `max`.
fn truncate_safe(s: &mut String, max: usize) {
    if s.len() > max {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
}

fn truncate_keep_tail_safe(s: &mut String, max: usize, prefix: &str) {
    if s.len() <= max {
        return;
    }

    let prefix = prefix.as_bytes();
    if prefix.len() >= max {
        truncate_safe(s, max);
        return;
    }

    let keep = max - prefix.len();
    let mut start = s.len().saturating_sub(keep);
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }

    let tail = s[start..].to_string();
    s.clear();
    s.push_str(std::str::from_utf8(prefix).unwrap_or_default());
    s.push_str(&tail);
}

/// Cap for replay buffer strings (128 KB). Keeps memory bounded for long outputs.
const LIVE_REPLAY_CAP: usize = 128 * 1024;
/// Max delegated events kept per round. Prevents unbounded memory growth for
/// long-running rounds with many sub-agent / orchestration events.
const DELEGATED_EVENTS_CAP: usize = 10_000;
const TOOL_PROGRESS_HEARTBEAT_SECS: u64 = 1;
const LIVE_CLIENT_SEND_BACKPRESSURE_TIMEOUT_MS: u64 = 25;
const LIVE_CLIENT_SEND_BACKPRESSURE_MAX_TIMEOUTS: usize = 4;

fn generate_secret_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| format!("failed to get secure random bytes for secret token: {e}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn generate_shutdown_token() -> Result<String, String> {
    generate_secret_token()
}

const GENERATED_SESSION_ID_LEN: usize = 6;
const GENERATED_SESSION_ID_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

fn generate_random_session_id() -> Result<String, String> {
    let mut id = String::with_capacity(GENERATED_SESSION_ID_LEN);
    while id.len() < GENERATED_SESSION_ID_LEN {
        let mut bytes = [0_u8; 16];
        getrandom::getrandom(&mut bytes)
            .map_err(|e| format!("failed to get secure random bytes for session id: {e}"))?;
        for byte in bytes {
            if byte >= 252 {
                continue;
            }
            let idx = (byte % GENERATED_SESSION_ID_ALPHABET.len() as u8) as usize;
            id.push(GENERATED_SESSION_ID_ALPHABET[idx] as char);
            if id.len() == GENERATED_SESSION_ID_LEN {
                break;
            }
        }
    }
    Ok(id)
}

fn forbidden_local_api(message: &str) -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::FORBIDDEN, Json(json!({"error": message})))
}

fn authority_host(header_value: &str) -> Option<String> {
    let trimmed = header_value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(stripped) = trimmed.strip_prefix('[') {
        let end = stripped.find(']')?;
        return Some(stripped[..end].to_string());
    }
    if trimmed.matches(':').count() == 1 {
        return trimmed.split_once(':').map(|(host, _)| host.to_string());
    }
    Some(trimmed.to_string())
}

fn is_loopback_or_localhost(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

fn validate_loopback_host_header(
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let host = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .and_then(authority_host)
        .ok_or_else(|| forbidden_local_api("Blocked non-local request: invalid Host header"))?;
    if !is_loopback_or_localhost(&host) {
        return Err(forbidden_local_api(
            "Blocked non-local request: Host header must target localhost or a loopback address",
        ));
    }
    Ok(())
}

pub(crate) fn validate_local_request_headers(
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    validate_loopback_host_header(headers)?;

    for header_name in ["origin", "referer"] {
        if let Some(value) = headers.get(header_name) {
            let origin = value.to_str().map_err(|_| {
                forbidden_local_api("Blocked non-local request: malformed Origin/Referer header")
            })?;
            let parsed = reqwest::Url::parse(origin).map_err(|_| {
                forbidden_local_api("Blocked non-local request: malformed Origin/Referer URL")
            })?;
            let origin_host = parsed.host_str().ok_or_else(|| {
                forbidden_local_api("Blocked non-local request: Origin/Referer URL has no host")
            })?;
            if !is_loopback_or_localhost(origin_host) {
                return Err(forbidden_local_api(
                    "Blocked non-local request: Origin/Referer must be localhost or a loopback address",
                ));
            }
        }
    }

    Ok(())
}

fn validate_local_request_for_route(
    method: &Method,
    path: &str,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if *method == Method::GET && path == "/api/mcp/auth/callback" {
        validate_loopback_host_header(headers)
    } else {
        validate_local_request_headers(headers)
    }
}

async fn enforce_local_request(
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_for_route(request.method(), request.uri().path(), request.headers())?;
    Ok(next.run(request).await)
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

pub(crate) fn strip_json_fences(content: &str) -> &str {
    let trimmed = content.trim();
    let Some(without_ticks) = trimmed.strip_prefix("```") else {
        return trimmed;
    };

    let body = if let Some((first_line, rest)) = without_ticks.split_once('\n') {
        let language = first_line.trim();
        if language.is_empty()
            || language
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        {
            rest
        } else {
            without_ticks
        }
    } else {
        without_ticks
    };

    body.trim()
        .strip_suffix("```")
        .unwrap_or(body.trim())
        .trim()
}

/// Tokenize a string into lowercase words for keyword matching.
/// CJK characters are emitted as individual tokens so that per-character
/// overlap scoring works with `text.contains(token)`.
fn tokenize_for_matching(s: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            current.push(c);
        } else if is_cjk_char(c) {
            if current.len() >= 2 {
                tokens.push(current.to_lowercase());
            }
            current.clear();
            tokens.push(c.to_string());
        } else {
            if current.len() >= 2 {
                tokens.push(current.to_lowercase());
            }
            current.clear();
        }
    }
    if current.len() >= 2 {
        tokens.push(current.to_lowercase());
    }
    tokens
}

/// Returns `true` for CJK Unified, Extension A, Compatibility,
/// Hiragana, Katakana, and Hangul Syllables.
fn is_cjk_char(c: char) -> bool {
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

pub(crate) type WsTx = mpsc::Sender<String>;
pub(crate) type LiveTx = mpsc::Sender<serde_json::Value>;
pub(crate) const LIVE_EVENT_CHANNEL_CAPACITY: usize = 256;
const MAX_PENDING_LIVE_CLIENT_EVENTS: usize = 1_024;

#[derive(Clone)]
pub(crate) struct LiveOutputReplayCtx {
    pub(crate) state: Arc<AppState>,
    pub(crate) session_id: String,
}

pub(crate) async fn ws_send(tx: &WsTx, data: &serde_json::Value) -> bool {
    tx.send(data.to_string()).await.is_ok()
}

async fn send_storage_status(tx: &WsTx, state: &AppState) {
    let _ = ws_send(
        tx,
        &json!({
            "type": "storage_status",
            "storage": storage_status_payload(state),
        }),
    )
    .await;
}

#[cfg(not(test))]
async fn broadcast_storage_status(state: &AppState) {
    let mut clients = state
        .session_clients
        .lock()
        .await
        .values()
        .map(|binding| binding.tx.clone())
        .collect::<Vec<_>>();
    clients.extend(
        state
            .group_clients
            .lock()
            .await
            .values()
            .map(|binding| binding.tx.clone()),
    );
    let payload = json!({
        "type": "storage_status",
        "storage": storage_status_payload(state),
    });
    for client in clients {
        let _ = ws_send(&client, &payload).await;
    }
}

#[cfg(not(test))]
async fn monitor_storage_status(
    state: Arc<AppState>,
    mut status_rx: tokio::sync::watch::Receiver<storage::StorageStatus>,
) {
    while status_rx.changed().await.is_ok() {
        if status_rx.borrow().mode != storage::StorageMode::Protected {
            continue;
        }
        let runs = state
            .active_runs
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for run in runs {
            run.cancel.cancel();
        }
        session_control::cancel_all_active_runs_for_storage();
        broadcast_storage_status(&state).await;
        break;
    }
}

pub(crate) async fn live_send(tx: &LiveTx, data: serde_json::Value) -> bool {
    tx.send(data).await.is_ok()
}

pub(crate) async fn send_session_client_event(
    state: &AppState,
    session_id: &str,
    event: serde_json::Value,
) {
    let Some(SessionClientSendBatch {
        connection_id,
        tx,
        events,
    }) = take_live_client_send_batch(
        state,
        session_id,
        queue_live_client_events(state, session_id, vec![event]).await,
    )
    .await
    else {
        return;
    };

    if send_live_client_events_to_writer(state, session_id, connection_id, &tx, events).await {
        finish_live_client_send(state, session_id, connection_id, Vec::new()).await;
    }
}

pub(crate) async fn send_group_client_event(
    state: &AppState,
    group_id: &str,
    event: session_control::GroupClientEvent,
) {
    let session_control::GroupClientEvent { payload, droppable } = event;
    let client = {
        let clients = state.group_clients.lock().await;
        clients
            .get(group_id)
            .map(|binding| (binding.connection_id, binding.tx.clone()))
    };
    let Some((connection_id, tx)) = client else {
        return;
    };
    match tx.try_send(payload.to_string()) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) if droppable => {}
        Err(TrySendError::Full(_)) | Err(TrySendError::Closed(_)) => {
            let stale = {
                let mut clients = state.group_clients.lock().await;
                if clients.get(group_id).map(|binding| binding.connection_id) == Some(connection_id)
                {
                    clients.remove(group_id)
                } else {
                    None
                }
            };
            if let Some(binding) = stale {
                binding.cancel.cancel();
            }
        }
    }
}

pub(crate) async fn close_group_client(state: &AppState, group_id: &str) {
    let binding = {
        let mut clients = state.group_clients.lock().await;
        clients.remove(group_id)
    };
    if let Some(binding) = binding {
        binding.cancel.cancel();
    }
}

pub(crate) async fn forward_tool_output_event_best_effort(
    live_tx: &LiveTx,
    event: serde_json::Value,
    replay_ctx: Option<&LiveOutputReplayCtx>,
) {
    if let Some(replay_ctx) = replay_ctx {
        record_tool_output_event_for_replay_and_client(
            replay_ctx.state.as_ref(),
            &replay_ctx.session_id,
            event,
        )
        .await;
    } else {
        let _ = live_tx.try_send(event);
    }
}

async fn queue_live_client_events(
    state: &AppState,
    session_id: &str,
    events: Vec<serde_json::Value>,
) -> Option<QueueLiveClientEventsResult> {
    let mut clients = state.session_clients.lock().await;
    let binding = clients.get_mut(session_id)?;

    queue_live_client_events_for_binding(binding, events)
}

struct SessionClientSendBatch {
    connection_id: u64,
    tx: WsTx,
    events: Vec<serde_json::Value>,
}

struct SlowClientDisconnect {
    connection_id: u64,
}

enum QueueLiveClientEventsResult {
    Batch(SessionClientSendBatch),
    Disconnect(SlowClientDisconnect),
}

fn queue_live_client_events_for_binding(
    binding: &mut SessionClientBinding,
    events: Vec<serde_json::Value>,
) -> Option<QueueLiveClientEventsResult> {
    if !events.is_empty()
        && binding.pending_events.len().saturating_add(events.len())
            > MAX_PENDING_LIVE_CLIENT_EVENTS
    {
        return Some(QueueLiveClientEventsResult::Disconnect(
            SlowClientDisconnect {
                connection_id: binding.connection_id,
            },
        ));
    }

    if !binding.replay_ready {
        binding.pending_events.extend(events);
        return None;
    }

    if binding.live_send_in_progress {
        binding.pending_events.extend(events);
        return None;
    }

    if binding.pending_events.is_empty() {
        if events.is_empty() {
            return None;
        }
        binding.live_send_in_progress = true;
        return Some(QueueLiveClientEventsResult::Batch(SessionClientSendBatch {
            connection_id: binding.connection_id,
            tx: binding.tx.clone(),
            events,
        }));
    }

    let mut queued = std::mem::take(&mut binding.pending_events);
    queued.extend(events);
    binding.live_send_in_progress = true;
    Some(QueueLiveClientEventsResult::Batch(SessionClientSendBatch {
        connection_id: binding.connection_id,
        tx: binding.tx.clone(),
        events: queued.into_iter().collect(),
    }))
}

async fn flush_queued_live_client_events(state: &AppState, session_id: &str, connection_id: u64) {
    loop {
        let next_batch = {
            let mut clients = state.session_clients.lock().await;
            let Some(binding) = clients.get_mut(session_id) else {
                return;
            };
            if binding.connection_id != connection_id {
                return;
            }
            if binding.live_send_in_progress
                || binding.pending_events.is_empty()
                || !binding.replay_ready
            {
                return;
            }
            binding.live_send_in_progress = true;
            Some((
                binding.tx.clone(),
                binding.pending_events.drain(..).collect::<Vec<_>>(),
            ))
        };

        let Some((tx, events)) = next_batch else {
            return;
        };

        if !send_live_client_events_to_writer(state, session_id, connection_id, &tx, events).await {
            return;
        }

        let mut clients = state.session_clients.lock().await;
        let Some(binding) = clients.get_mut(session_id) else {
            return;
        };
        if binding.connection_id != connection_id {
            return;
        }
        binding.live_send_in_progress = false;
    }
}

async fn finish_live_client_send(
    state: &AppState,
    session_id: &str,
    connection_id: u64,
    unsent_events: Vec<serde_json::Value>,
) {
    let should_flush = {
        let mut clients = state.session_clients.lock().await;
        let Some(binding) = clients.get_mut(session_id) else {
            return;
        };
        if binding.connection_id != connection_id {
            return;
        }
        binding.live_send_in_progress = false;
        if !unsent_events.is_empty() {
            let mut remainder: VecDeque<serde_json::Value> = unsent_events.into();
            remainder.append(&mut binding.pending_events);
            binding.pending_events = remainder;
            false
        } else {
            !binding.pending_events.is_empty() && binding.replay_ready
        }
    };

    if should_flush {
        flush_queued_live_client_events(state, session_id, connection_id).await;
    }
}

async fn send_live_client_events_to_writer(
    state: &AppState,
    session_id: &str,
    connection_id: u64,
    tx: &WsTx,
    events: Vec<serde_json::Value>,
) -> bool {
    let mut unsent_start = 0usize;
    let mut consecutive_timeouts = 0usize;
    while unsent_start < events.len() {
        let payload = events[unsent_start].to_string();
        match tx.try_send(payload.clone()) {
            Ok(()) => {
                consecutive_timeouts = 0;
                unsent_start += 1;
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                match tx
                    .send_timeout(
                        payload,
                        Duration::from_millis(LIVE_CLIENT_SEND_BACKPRESSURE_TIMEOUT_MS),
                    )
                    .await
                {
                    Ok(()) => {
                        consecutive_timeouts = 0;
                        unsent_start += 1;
                    }
                    Err(tokio::sync::mpsc::error::SendTimeoutError::Timeout(_)) => {
                        consecutive_timeouts += 1;
                        if consecutive_timeouts >= LIVE_CLIENT_SEND_BACKPRESSURE_MAX_TIMEOUTS {
                            disconnect_session_connection_if_matches(
                                state,
                                session_id,
                                connection_id,
                            )
                            .await;
                            return false;
                        }
                    }
                    Err(tokio::sync::mpsc::error::SendTimeoutError::Closed(_)) => {
                        unbind_session_connection_if_matches(state, session_id, connection_id)
                            .await;
                        return false;
                    }
                }
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                unbind_session_connection_if_matches(state, session_id, connection_id).await;
                return false;
            }
        }
    }
    true
}

async fn take_live_client_events_for_send(
    state: &AppState,
    session_id: &str,
    connection_id: u64,
    event: serde_json::Value,
) -> Option<QueueLiveClientEventsResult> {
    let mut clients = state.session_clients.lock().await;
    let binding = clients.get_mut(session_id)?;
    if binding.connection_id != connection_id {
        return None;
    }

    queue_live_client_events_for_binding(binding, vec![event])
}

async fn record_tool_output_event_for_replay_and_client(
    state: &AppState,
    session_id: &str,
    event: serde_json::Value,
) {
    let mut client_events: Vec<serde_json::Value> = Vec::new();
    let source_connection_id = {
        let active_runs = state.active_runs.lock().await;
        active_runs
            .get(session_id)
            .map(|binding| binding.connection_id)
    };
    let source_connection_id = if source_connection_id.is_some() {
        source_connection_id
    } else {
        let live_rounds = state.live_rounds.lock().await;
        live_rounds.get(session_id).map(|round| round.connection_id)
    };

    if let Some(connection_id) = source_connection_id {
        let mut live_rounds = state.live_rounds.lock().await;
        if let Some(round) = live_rounds.get_mut(session_id)
            && round.connection_id == connection_id
        {
            if is_subagent_live_event(&event) {
                if let Some(task_key) = live_task_key_from_event(&event) {
                    if !round.active_tasks.contains(&task_key) {
                        let start_event = synthetic_task_started_event_for_output(&event);
                        let can_store_task_start =
                            round.delegated_events.len() < DELEGATED_EVENTS_CAP;
                        let mut recorded_orchestrate_start = false;
                        if let Some(orchestrate_started_event) =
                            synthetic_orchestrate_started_event_for_output(
                                &event,
                                &round.delegated_events,
                                Some(&round.active_tasks),
                            )
                        {
                            let orchestrate_id = orchestrate_started_event["orchestrate_id"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string();
                            if !orchestrate_id.is_empty()
                                && round.delegated_events.len() < DELEGATED_EVENTS_CAP
                            {
                                if let Some(existing) =
                                    round.delegated_events.iter_mut().rev().find(|existing| {
                                        existing["type"] == "orchestrate_started"
                                            && existing["synthetic"].as_bool().unwrap_or(false)
                                            && existing["orchestrate_id"].as_str()
                                                == Some(orchestrate_id.as_str())
                                    })
                                {
                                    *existing = orchestrate_started_event.clone();
                                    client_events.push(orchestrate_started_event);
                                } else {
                                    round.active_orchestrations.insert(orchestrate_id);
                                    round
                                        .delegated_events
                                        .push(orchestrate_started_event.clone());
                                    client_events.push(orchestrate_started_event);
                                    recorded_orchestrate_start = true;
                                }
                            }
                        }
                        if let Some(start_event) = start_event
                            && can_store_task_start
                            && round.delegated_events.len() < DELEGATED_EVENTS_CAP
                        {
                            round.active_tasks.insert(task_key.clone());
                            round.delegated_events.push(start_event.clone());
                            client_events.push(start_event);
                        } else if recorded_orchestrate_start {
                            round.active_tasks.insert(task_key);
                        }
                    }
                    if round.delegated_events.len() < DELEGATED_EVENTS_CAP {
                        round.delegated_events.push(event.clone());
                    }
                }
            } else if !is_subagent_live_event(&event) {
                let tool_id = event["id"].as_str().unwrap_or_default();
                let chunk = event["chunk"].as_str().unwrap_or_default();
                let stream = event["stream"].as_str();
                if let Some(tool) = round.tools.iter_mut().find(|tool| tool.id == tool_id) {
                    merge_live_tool_output(&mut tool.live_output, stream, chunk);
                    if tool.arguments.is_empty()
                        && let Some(tool_call_event) = synthetic_tool_call_event_for_output(&event)
                    {
                        client_events.push(tool_call_event);
                    }
                } else {
                    if let Some(tool_call_event) = synthetic_tool_call_event_for_output(&event) {
                        client_events.push(tool_call_event);
                    }
                    let mut live_output = String::new();
                    merge_live_tool_output(&mut live_output, stream, chunk);
                    round.tools.push(LiveToolState {
                        id: tool_id.to_string(),
                        name: event["name"].as_str().unwrap_or_default().to_string(),
                        arguments: String::new(),
                        live_output,
                        result: None,
                        elapsed_ms: 0,
                        images: Vec::new(),
                        is_error: false,
                    });
                }
            }
        }
    }

    client_events.push(event);

    let Some(SessionClientSendBatch {
        connection_id,
        tx,
        events,
    }) = take_live_client_send_batch(
        state,
        session_id,
        queue_live_client_events(state, session_id, client_events).await,
    )
    .await
    else {
        return;
    };

    if send_live_client_events_to_writer(state, session_id, connection_id, &tx, events).await {
        finish_live_client_send(state, session_id, connection_id, Vec::new()).await;
    }
}

pub(crate) async fn replace_connection_cancel_binding(
    state: &AppState,
    session_id: &str,
    connection_id: u64,
    connection_cancel: &CancellationToken,
) {
    let old_binding = {
        let mut cancels = state.connection_cancels.lock().await;
        let old_binding = cancels.remove(session_id);
        cancels.insert(
            session_id.to_string(),
            ConnectionCancelBinding {
                connection_id,
                cancel: connection_cancel.clone(),
            },
        );
        old_binding
    };

    if let Some(old_binding) = old_binding
        && old_binding.connection_id != connection_id
    {
        old_binding.cancel.cancel();
    }
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
            pending_events: VecDeque::new(),
            live_send_in_progress: false,
        },
    );
}

async fn finish_session_replay(state: &AppState, session_id: &str, connection_id: u64) {
    let should_flush = {
        let mut clients = state.session_clients.lock().await;
        let Some(binding) = clients.get_mut(session_id) else {
            return;
        };
        if binding.connection_id != connection_id {
            return;
        }

        binding.replay_ready = true;
        !binding.live_send_in_progress && !binding.pending_events.is_empty()
    };

    if should_flush {
        flush_queued_live_client_events(state, session_id, connection_id).await;
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

async fn disconnect_session_connection_if_matches(
    state: &AppState,
    session_id: &str,
    connection_id: u64,
) {
    let cancel = {
        let cancels = state.connection_cancels.lock().await;
        cancels
            .get(session_id)
            .filter(|binding| binding.connection_id == connection_id)
            .map(|binding| binding.cancel.clone())
    };
    if let Some(cancel) = cancel {
        cancel.cancel();
    }
    unbind_session_connection_if_matches(state, session_id, connection_id).await;
}

async fn take_live_client_send_batch(
    state: &AppState,
    session_id: &str,
    result: Option<QueueLiveClientEventsResult>,
) -> Option<SessionClientSendBatch> {
    match result? {
        QueueLiveClientEventsResult::Batch(batch) => Some(batch),
        QueueLiveClientEventsResult::Disconnect(SlowClientDisconnect { connection_id }) => {
            disconnect_session_connection_if_matches(state, session_id, connection_id).await;
            None
        }
    }
}

fn apply_live_compression_event(
    round: &mut LiveRoundState,
    event_type: &str,
    event: &serde_json::Value,
) {
    match event_type {
        "context_compressed" => {
            round.latest_compression.outcome = Some("compressed".to_string());
            round.latest_compression.reason = None;
            round.latest_compression.messages_removed = event["messages_removed"]
                .as_u64()
                .map(|value| value as usize);
            round.latest_compression.before_estimate = event["before_estimate"]
                .as_u64()
                .map(|value| value as usize);
            round.latest_compression.after_estimate =
                event["after_estimate"].as_u64().map(|value| value as usize);
            round.latest_compression.saved_tokens =
                event["saved_tokens"].as_u64().map(|value| value as usize);
            round.latest_compression.saved_percent =
                event["saved_percent"].as_u64().map(|value| value as usize);
            round.latest_compression.pruned_messages_removed = None;
        }
        "context_compress_skipped" => {
            round.latest_compression.outcome = Some("skipped".to_string());
            round.latest_compression.reason = event["reason"].as_str().map(str::to_string);
            round.latest_compression.messages_removed = None;
            round.latest_compression.before_estimate = None;
            round.latest_compression.after_estimate = None;
            round.latest_compression.saved_tokens = None;
            round.latest_compression.saved_percent = None;
            round.latest_compression.pruned_messages_removed = None;
        }
        "context_compress_failed" => {
            round.latest_compression.outcome = Some("failed".to_string());
            round.latest_compression.reason = event["error"].as_str().map(str::to_string);
            round.latest_compression.messages_removed = None;
            round.latest_compression.before_estimate = None;
            round.latest_compression.after_estimate = None;
            round.latest_compression.saved_tokens = None;
            round.latest_compression.saved_percent = None;
            round.latest_compression.pruned_messages_removed = None;
        }
        _ => {}
    }
}

fn apply_live_pruned_event(round: &mut LiveRoundState, event: &serde_json::Value) {
    round.latest_compression.pruned_messages_removed = event["messages_removed"]
        .as_u64()
        .map(|value| value as usize);
}

fn clear_live_compression_state(round: &mut LiveRoundState) {
    round.latest_compression = LiveCompressionState::default();
    round.has_pending_pre_start_context_updates = false;
}

async fn dispatch_live_event(
    state: &AppState,
    session_id: &str,
    connection_id: u64,
    event: serde_json::Value,
) {
    let event_type = event["type"].as_str().unwrap_or_default();
    let mut delegated_replay_event: Option<serde_json::Value> = None;
    let active_run_connection_id = {
        let runs = state.active_runs.lock().await;
        runs.get(session_id).map(|binding| binding.connection_id)
    };
    let live_round_connection_id = {
        let live_rounds = state.live_rounds.lock().await;
        live_rounds.get(session_id).map(|round| round.connection_id)
    };

    // Validate connection ownership first, then update live replay state.
    // Keep session_clients and live_rounds lock acquisition ordered and
    // non-overlapping so live tool-output updates cannot deadlock with normal
    // websocket event processing.
    let current_connection_id = {
        let clients_guard = state.session_clients.lock().await;
        clients_guard
            .get(session_id)
            .map(|binding| binding.connection_id)
    };
    let is_current = current_connection_id == Some(connection_id);
    let is_active_run_source = active_run_connection_id == Some(connection_id);
    let is_live_round_source = live_round_connection_id == Some(connection_id);
    if !(is_current || is_active_run_source || is_live_round_source) {
        return;
    }

    {
        let mut live_rounds = state.live_rounds.lock().await;

        match event_type {
            "start" => {
                if !is_subagent_live_event(&event) {
                    let latest_compression = live_rounds
                        .get(session_id)
                        .filter(|round| round.has_pending_pre_start_context_updates)
                        .map(|round| round.latest_compression.clone())
                        .unwrap_or_default();
                    live_rounds.insert(
                        session_id.to_string(),
                        LiveRoundState {
                            connection_id,
                            round: event["round"].as_u64().unwrap_or(1) as usize,
                            react_visible: event["react_visible"].as_bool().unwrap_or(false),
                            phase: event["phase"].as_str().map(str::to_string),
                            cycle: event["cycle"].as_u64().map(|value| value as usize),
                            effective_model: event["model"].as_str().map(str::to_string),
                            effective_think: event["think_level"].as_str().map(str::to_string),
                            run_mode: event["run_mode"].as_str().map(str::to_string),
                            auto_observation_strength: event["auto_observation_strength"]
                                .as_str()
                                .map(str::to_string),
                            auto_stagnation_streak: event["auto_stagnation_streak"]
                                .as_u64()
                                .map(|value| value as usize),
                            auto_error_streak: event["auto_error_streak"]
                                .as_u64()
                                .map(|value| value as usize),
                            auto_task_pressure: event["auto_task_pressure"]
                                .as_u64()
                                .map(|value| value as usize),
                            auto_action_oriented: event["auto_action_oriented"].as_bool(),
                            auto_ready_to_finish: event["auto_ready_to_finish"].as_bool(),
                            auto_has_blocking_uncertainty: event["auto_has_blocking_uncertainty"]
                                .as_bool(),
                            latest_auto_trace: None,
                            latest_task_plan: None,
                            latest_compression,
                            has_pending_pre_start_context_updates: false,
                            has_observation: false,
                            assistant_text: String::new(),
                            reasoning_text: String::new(),
                            reasoning_done: false,
                            tools: Vec::new(),
                            delegated_events: Vec::new(),
                            active_tasks: HashSet::new(),
                            active_orchestrations: HashSet::new(),
                        },
                    );
                }
            }
            "delta" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                    && let Some(content) = event["content"].as_str()
                    && round.assistant_text.len() < LIVE_REPLAY_CAP
                {
                    round.assistant_text.push_str(content);
                    truncate_safe(&mut round.assistant_text, LIVE_REPLAY_CAP);
                }
            }
            "auto_trace" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                    && let Ok(trace) =
                        serde_json::from_value::<agent::AutoThinkTrace>(event.clone())
                {
                    round.effective_think = Some(trace.selected_think.clone());
                    round.latest_auto_trace = Some(trace);
                }
            }
            "task_plan" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                {
                    round.latest_task_plan = Some(event.clone());
                }
            }
            "context_compressed" | "context_compress_skipped" | "context_compress_failed" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                {
                    apply_live_compression_event(round, event_type, &event);
                    round.has_pending_pre_start_context_updates = true;
                } else if !is_subagent_live_event(&event) {
                    let mut round = live_rounds.remove(session_id).unwrap_or_default();
                    round.connection_id = connection_id;
                    apply_live_compression_event(&mut round, event_type, &event);
                    round.has_pending_pre_start_context_updates = true;
                    live_rounds.insert(session_id.to_string(), round);
                }
            }
            "context_pruned" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                {
                    apply_live_pruned_event(round, &event);
                    round.has_pending_pre_start_context_updates = true;
                } else if !is_subagent_live_event(&event) {
                    let mut round = live_rounds.remove(session_id).unwrap_or_default();
                    round.connection_id = connection_id;
                    apply_live_pruned_event(&mut round, &event);
                    round.has_pending_pre_start_context_updates = true;
                    live_rounds.insert(session_id.to_string(), round);
                }
            }
            "thinking_start" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                {
                    if is_subagent_live_event(&event)
                        && let Some(task_key) = live_task_key_from_event(&event)
                        && round.active_tasks.contains(&task_key)
                    {
                        delegated_replay_event = Some(event.clone());
                    } else if !is_subagent_live_event(&event) {
                        round.reasoning_text.clear();
                        round.reasoning_done = false;
                    }
                }
            }
            "thinking_delta" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                {
                    if is_subagent_live_event(&event)
                        && let Some(task_key) = live_task_key_from_event(&event)
                        && round.active_tasks.contains(&task_key)
                    {
                        delegated_replay_event = Some(event.clone());
                    } else if !is_subagent_live_event(&event)
                        && let Some(content) = event["content"].as_str()
                        && round.reasoning_text.len() < LIVE_REPLAY_CAP
                    {
                        round.reasoning_text.push_str(content);
                        truncate_safe(&mut round.reasoning_text, LIVE_REPLAY_CAP);
                    }
                }
            }
            "thinking_done" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                {
                    if is_subagent_live_event(&event)
                        && let Some(task_key) = live_task_key_from_event(&event)
                        && round.active_tasks.contains(&task_key)
                    {
                        delegated_replay_event = Some(event.clone());
                    } else if !is_subagent_live_event(&event) {
                        round.reasoning_done = true;
                    }
                }
            }
            "tool_call" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                {
                    let tool_id = event["id"].as_str().unwrap_or_default();
                    let incoming_arguments = event["arguments"].as_str().unwrap_or_default();
                    let is_synthetic = event["synthetic"].as_bool().unwrap_or(false);
                    if let Some(tool) = round.tools.iter_mut().find(|tool| tool.id == tool_id) {
                        tool.name = event["name"].as_str().unwrap_or_default().to_string();
                        if !incoming_arguments.is_empty()
                            || tool.arguments.is_empty()
                            || !is_synthetic
                        {
                            tool.arguments = incoming_arguments.to_string();
                        }
                    } else {
                        round.tools.push(LiveToolState {
                            id: tool_id.to_string(),
                            name: event["name"].as_str().unwrap_or_default().to_string(),
                            arguments: incoming_arguments.to_string(),
                            live_output: String::new(),
                            result: None,
                            elapsed_ms: 0,
                            images: Vec::new(),
                            is_error: false,
                        });
                    }
                }
            }
            "tool_output" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                {
                    if is_subagent_live_event(&event)
                        && let Some(task_key) = live_task_key_from_event(&event)
                        && round.active_tasks.contains(&task_key)
                    {
                        delegated_replay_event = Some(event.clone());
                    } else if !is_subagent_live_event(&event) {
                        let tool_id = event["id"].as_str().unwrap_or_default();
                        let chunk = event["chunk"].as_str().unwrap_or_default();
                        let stream = event["stream"].as_str();
                        if let Some(tool) = round.tools.iter_mut().find(|tool| tool.id == tool_id) {
                            merge_live_tool_output(&mut tool.live_output, stream, chunk);
                        } else {
                            let mut live_output = String::new();
                            merge_live_tool_output(&mut live_output, stream, chunk);
                            round.tools.push(LiveToolState {
                                id: tool_id.to_string(),
                                name: event["name"].as_str().unwrap_or_default().to_string(),
                                arguments: String::new(),
                                live_output,
                                result: None,
                                elapsed_ms: 0,
                                images: Vec::new(),
                                is_error: false,
                            });
                        }
                    }
                }
            }
            "tool_progress" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                {
                    let tool_id = event["id"].as_str().unwrap_or_default();
                    let elapsed_ms = event["elapsed_ms"].as_u64().unwrap_or(0);
                    if let Some(tool) = round.tools.iter_mut().find(|tool| tool.id == tool_id) {
                        tool.elapsed_ms = elapsed_ms;
                    } else {
                        round.tools.push(LiveToolState {
                            id: tool_id.to_string(),
                            name: event["name"].as_str().unwrap_or_default().to_string(),
                            arguments: String::new(),
                            live_output: String::new(),
                            result: None,
                            elapsed_ms,
                            images: Vec::new(),
                            is_error: false,
                        });
                    }
                }
            }
            "tool_result" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                {
                    if is_subagent_live_event(&event)
                        && let Some(task_key) = live_task_key_from_event(&event)
                        && round.active_tasks.contains(&task_key)
                    {
                        let replay_event = truncated_live_tool_result_event(&event);
                        delegated_replay_event = Some(replay_event);
                    } else if !is_subagent_live_event(&event) {
                        let tool_id = event["id"].as_str().unwrap_or_default();
                        let mut result = event["result"].as_str().unwrap_or_default().to_string();
                        truncate_safe(&mut result, LIVE_REPLAY_CAP);
                        let images = event["images"].as_array().cloned().unwrap_or_default();
                        let is_error = event["is_error"].as_bool().unwrap_or(false);
                        if let Some(tool) = round.tools.iter_mut().find(|tool| tool.id == tool_id) {
                            tool.result = Some(result);
                            tool.elapsed_ms =
                                event["duration_ms"].as_u64().unwrap_or(tool.elapsed_ms);
                            tool.images = images;
                            tool.is_error = is_error;
                        } else {
                            round.tools.push(LiveToolState {
                                id: tool_id.to_string(),
                                name: event["name"].as_str().unwrap_or_default().to_string(),
                                arguments: String::new(),
                                live_output: String::new(),
                                result: Some(result),
                                elapsed_ms: event["duration_ms"].as_u64().unwrap_or(0),
                                images,
                                is_error,
                            });
                        }
                    }
                }
            }
            "react_phase" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                {
                    let next_phase = event["phase"].as_str().map(str::to_string);
                    let next_cycle = event["cycle"].as_u64().map(|value| value as usize);
                    let is_new_analyze_cycle = matches!(next_phase.as_deref(), Some("analyze"))
                        && next_cycle
                            .zip(round.cycle)
                            .is_some_and(|(next, current)| next > current);
                    if is_new_analyze_cycle {
                        clear_live_compression_state(round);
                    }
                    round.phase = next_phase;
                    round.cycle = next_cycle;
                }
            }
            "observation" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                {
                    round.has_observation = true;
                }
            }
            "task_started" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(task_key) = live_task_key_from_event(&event)
                {
                    if round.active_tasks.contains(&task_key) {
                        if let Some(existing) =
                            round.delegated_events.iter_mut().rev().find(|existing| {
                                existing["type"] == "task_started"
                                    && live_task_key_from_event(existing).as_deref()
                                        == Some(task_key.as_str())
                                    && existing["prompt"].as_str().unwrap_or_default().is_empty()
                            })
                        {
                            *existing = event.clone();
                        }
                    } else {
                        delegated_replay_event = Some(event.clone());
                    }
                }
            }
            "task_progress" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(task_key) = live_task_key_from_event(&event)
                    && round.active_tasks.contains(&task_key)
                {
                    delegated_replay_event = Some(event.clone());
                }
            }
            "task_tool" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(task_key) = live_task_key_from_event(&event)
                    && round.active_tasks.contains(&task_key)
                {
                    delegated_replay_event = Some(event.clone());
                }
            }
            "task_completed" | "task_failed" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(task_key) = live_task_key_from_event(&event)
                    && round.active_tasks.remove(&task_key)
                {
                    delegated_replay_event = Some(event.clone());
                }
            }
            "orchestrate_started" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && event["orchestrate_id"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty())
                {
                    delegated_replay_event = Some(event.clone());
                }
            }
            "orchestrate_layer" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(orchestrate_id) = event["orchestrate_id"].as_str()
                    && round.active_orchestrations.contains(orchestrate_id)
                {
                    delegated_replay_event = Some(event.clone());
                }
            }
            // Orchestration events: track per-task lifecycle for live replay
            "orchestrate_task_started" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(orchestrate_id) = event["orchestrate_id"].as_str()
                    && round.active_orchestrations.contains(orchestrate_id)
                    && event["id"].as_str().is_some_and(|value| !value.is_empty())
                {
                    delegated_replay_event = Some(event.clone());
                }
            }
            "orchestrate_task_completed"
            | "orchestrate_task_failed"
            | "orchestrate_task_skipped" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(orchestrate_id) = event["orchestrate_id"].as_str()
                    && round.active_orchestrations.contains(orchestrate_id)
                    && let Some(task_id) = event["id"].as_str().filter(|value| !value.is_empty())
                {
                    let task_key = format!("{orchestrate_id}:{task_id}");
                    if round.active_tasks.remove(&task_key) {
                        delegated_replay_event = Some(event.clone());
                    }
                }
            }
            "orchestrate_completed" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(orchestrate_id) = event["orchestrate_id"].as_str()
                    && round.active_orchestrations.remove(orchestrate_id)
                {
                    let prefix = format!("{orchestrate_id}:");
                    round
                        .active_tasks
                        .retain(|task_key| !task_key.starts_with(&prefix));
                    delegated_replay_event = Some(event.clone());
                }
            }
            "done" | "error" => {
                if live_rounds.get(session_id).map(|r| r.connection_id) == Some(connection_id) {
                    live_rounds.remove(session_id);
                }
            }
            _ => {}
        }

        if let Some(replay_event) = delegated_replay_event
            && let Some(round) = live_rounds.get_mut(session_id)
            && round.connection_id == connection_id
        {
            if round.delegated_events.len() < DELEGATED_EVENTS_CAP {
                // Under soft cap — store and register lifecycle opens so
                // terminal events arriving after the cap can still close
                // them. Total memory is bounded at ≤ 2 × DELEGATED_EVENTS_CAP.
                match replay_event["type"].as_str().unwrap_or_default() {
                    "task_started" => {
                        if let Some(key) = live_task_key_from_event(&replay_event) {
                            round.active_tasks.insert(key);
                        }
                    }
                    "orchestrate_task_started" => {
                        if let Some(orchestrate_id) = replay_event["orchestrate_id"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                            && let Some(task_id) =
                                replay_event["id"].as_str().filter(|v| !v.is_empty())
                        {
                            round
                                .active_tasks
                                .insert(format!("{orchestrate_id}:{task_id}"));
                        }
                    }
                    "orchestrate_started" => {
                        if let Some(id) = replay_event["orchestrate_id"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                        {
                            round.active_orchestrations.insert(id.to_string());
                        }
                    }
                    _ => {}
                }
                round.delegated_events.push(replay_event);
            } else {
                // Over soft cap — only store terminal events whose lifecycle
                // open was recorded (active_tasks / active_orchestrations
                // guards in the match arms above already ensure this).
                let is_terminal = matches!(
                    replay_event["type"].as_str().unwrap_or_default(),
                    "task_completed"
                        | "task_failed"
                        | "orchestrate_completed"
                        | "orchestrate_task_completed"
                        | "orchestrate_task_failed"
                        | "orchestrate_task_skipped"
                );
                if is_terminal {
                    round.delegated_events.push(replay_event);
                }
            }
        }

        drop(live_rounds);
    }

    let target_connection_id = current_connection_id.unwrap_or(connection_id);
    let Some(SessionClientSendBatch {
        connection_id,
        tx,
        events,
    }) = take_live_client_send_batch(
        state,
        session_id,
        take_live_client_events_for_send(state, session_id, target_connection_id, event).await,
    )
    .await
    else {
        return;
    };

    if send_live_client_events_to_writer(state, session_id, connection_id, &tx, events).await {
        finish_live_client_send(state, session_id, connection_id, Vec::new()).await;
    }
}

fn compression_replay_event(live_round: &LiveRoundState) -> Option<serde_json::Value> {
    match live_round.latest_compression.outcome.as_deref()? {
        "compressed" => Some(json!({
            "type": "context_compressed",
            "messages_removed": live_round.latest_compression.messages_removed,
            "before_estimate": live_round.latest_compression.before_estimate,
            "after_estimate": live_round.latest_compression.after_estimate,
            "saved_tokens": live_round.latest_compression.saved_tokens,
            "saved_percent": live_round.latest_compression.saved_percent,
        })),
        "skipped" => Some(json!({
            "type": "context_compress_skipped",
            "reason": live_round.latest_compression.reason,
        })),
        "failed" => Some(json!({
            "type": "context_compress_failed",
            "error": live_round.latest_compression.reason,
        })),
        _ => None,
    }
}

fn compression_pruned_replay_event(live_round: &LiveRoundState) -> Option<serde_json::Value> {
    Some(json!({
        "type": "context_pruned",
        "messages_removed": live_round.latest_compression.pruned_messages_removed?,
    }))
}

fn live_round_replay_events(live_round: &LiveRoundState) -> Vec<serde_json::Value> {
    let mut start_event = json!({
        "type":"start",
        "round": live_round.round,
        "phase": live_round.phase.as_deref().unwrap_or("analyze"),
        "cycle": live_round.cycle,
        "think_level": live_round.effective_think,
        "react_visible": live_round.react_visible,
    });
    if let Some(start_obj) = start_event.as_object_mut() {
        if let Some(value) = live_round.effective_model.as_ref() {
            start_obj.insert("model".to_string(), json!(value));
        }
        if let Some(value) = live_round.run_mode.as_ref() {
            start_obj.insert("run_mode".to_string(), json!(value));
        }
        if let Some(value) = live_round.auto_observation_strength.as_ref() {
            start_obj.insert("auto_observation_strength".to_string(), json!(value));
        }
        if let Some(value) = live_round.auto_stagnation_streak {
            start_obj.insert("auto_stagnation_streak".to_string(), json!(value));
        }
        if let Some(value) = live_round.auto_error_streak {
            start_obj.insert("auto_error_streak".to_string(), json!(value));
        }
        if let Some(value) = live_round.auto_task_pressure {
            start_obj.insert("auto_task_pressure".to_string(), json!(value));
        }
        if let Some(value) = live_round.auto_action_oriented {
            start_obj.insert("auto_action_oriented".to_string(), json!(value));
        }
        if let Some(value) = live_round.auto_ready_to_finish {
            start_obj.insert("auto_ready_to_finish".to_string(), json!(value));
        }
        if let Some(value) = live_round.auto_has_blocking_uncertainty {
            start_obj.insert("auto_has_blocking_uncertainty".to_string(), json!(value));
        }
    }

    let mut events = Vec::new();
    let compression_event = compression_replay_event(live_round);
    let pruned_event = compression_pruned_replay_event(live_round);
    if let Some(event) = compression_event {
        events.push(event);
    }
    if let Some(event) = pruned_event {
        events.push(event);
    }
    events.push(start_event);
    if let Some(event) = live_round.latest_task_plan.as_ref() {
        events.push(event.clone());
    }
    if let Some(trace) = live_round.latest_auto_trace.as_ref() {
        events.push(trace.to_live_event());
    }

    if !live_round.reasoning_text.is_empty() {
        events.push(json!({"type":"thinking_start"}));
        events.push(json!({"type":"thinking_delta","content": live_round.reasoning_text}));
        if live_round.reasoning_done {
            events.push(json!({"type":"thinking_done"}));
        }
    }

    for tool in &live_round.tools {
        events.push(json!({
            "type":"tool_call",
            "id": tool.id,
            "name": tool.name,
            "arguments": tool.arguments,
        }));
        if !tool.live_output.is_empty() {
            events.push(json!({
                "type":"tool_output",
                "id": tool.id,
                "name": tool.name,
                "chunk": tool.live_output,
            }));
        }
        if tool.result.is_none() && tool.elapsed_ms > 0 {
            events.push(json!({
                "type":"tool_progress",
                "id": tool.id,
                "name": tool.name,
                "elapsed_ms": tool.elapsed_ms,
            }));
        }
        if let Some(result) = &tool.result {
            let mut result_event = json!({
                "type":"tool_result",
                "id": tool.id,
                "name": tool.name,
                "result": result,
                "duration_ms": tool.elapsed_ms,
                "is_error": tool.is_error,
            });
            if !tool.images.is_empty() {
                result_event["images"] = json!(tool.images);
            }
            events.push(result_event);
        }
    }

    if !live_round.assistant_text.is_empty() {
        events.push(json!({"type":"delta","content": live_round.assistant_text}));
    }

    for event in &live_round.delegated_events {
        events.push(event.clone());
    }

    events
}

async fn replay_live_round(tx: &WsTx, state: &AppState, session_id: &str) {
    let live_round = { state.live_rounds.lock().await.get(session_id).cloned() };
    let Some(live_round) = live_round else {
        return;
    };

    for event in live_round_replay_events(&live_round) {
        ws_send(tx, &event).await;
    }
}

async fn replay_group_member_live_round(
    tx: &WsTx,
    state: &AppState,
    group_id: &str,
    run: &session_group::GroupRun,
) {
    if run.status != "running" {
        return;
    }
    let live_round = { state.live_rounds.lock().await.get(&run.session_id).cloned() };
    let Some(live_round) = live_round else {
        return;
    };

    for event in live_round_replay_events(&live_round) {
        ws_send(
            tx,
            &json!({
                "type": "group_member_event",
                "group_id": group_id,
                "run_id": run.id,
                "session_id": run.session_id,
                "event": event,
            }),
        )
        .await;
    }
}

// ── WebSocket Handler ────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SessionQuery {
    #[serde(default)]
    session: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionSkillsUpdateRequest {
    enabled_system_skills: Vec<String>,
    known_system_skills: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpSessionPolicyUpdateRequest {
    enabled_servers: Vec<String>,
    enabled_tools: Vec<String>,
    #[serde(default)]
    confirm_mutating_tools: bool,
    #[serde(default)]
    client_capabilities: tools::mcp::McpClientCapabilityPolicy,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpServerRequest {
    server: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpAuthCallbackRequest {
    server: String,
    code: String,
    state: String,
}

#[derive(Deserialize)]
struct McpAuthCallbackQuery {
    server: String,
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpResourceReadRequest {
    server: String,
    uri: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct McpPromptGetRequest {
    server: String,
    name: String,
    #[serde(default)]
    arguments: serde_json::Value,
}

#[derive(Deserialize)]
struct SessionRenameRequest {
    name: String,
}

#[derive(Deserialize)]
struct WsSessionQuery {
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    group: Option<String>,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(query): Query<WsSessionQuery>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        if let Some(group_id) = query.group {
            handle_group_socket(socket, state, group_id, query.session).await;
        } else {
            handle_socket(socket, state, query.session).await;
        }
    })
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
    // Reader task: use bounded buffering for user text while allowing /stop to
    // bypass backlog via an atomic flag.
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<String>(INBOUND_BUFFER_CAPACITY);
    let stop_requested = Arc::new(AtomicBool::new(false));
    let run_active = Arc::new(AtomicBool::new(false));
    let reader_stop_requested = stop_requested.clone();
    let reader_run_active = run_active.clone();
    let reader_cancel = connection_cancel.clone();
    let reader = tokio::spawn(async move {
        loop {
            let Some(result) = (tokio::select! {
                biased;
                _ = reader_cancel.cancelled() => None,
                result = rx.next() => result,
            }) else {
                break;
            };
            match result {
                Ok(WsMsg::Text(t)) => {
                    if t.trim().eq_ignore_ascii_case("/stop")
                        && reader_run_active.load(Ordering::Relaxed)
                    {
                        reader_stop_requested.store(true, Ordering::Relaxed);
                        continue;
                    }
                    if inbound_tx.send(t.to_string()).await.is_err() {
                        break;
                    }
                }
                Ok(WsMsg::Close(_)) | Err(_) => break,
                _ => continue,
            }
        }
        reader_cancel.cancel();
    });

    let mut current_session_id = resolve_or_create_socket_session(
        &state,
        &tx,
        requested_id.as_deref(),
        connection_id,
        &connection_cancel,
    )
    .await;
    send_storage_status(&tx, &state).await;

    let cancel = state.shutdown.clone();
    let current_session_ref = Arc::new(Mutex::new(current_session_id.clone()));
    let (live_tx, socket_tasks) = spawn_connection_tasks(
        state.clone(),
        connection_cancel.clone(),
        current_session_ref.clone(),
        connection_id,
    );

    let mut rerun_agent = false;
    let mut agent_run_mode = runtime_loop::AgentRunMode::Execute;
    let mut agent_run_reservation = None;
    let mut agent_model_snapshot = None;
    loop {
        if !rerun_agent {
            let text = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                _ = connection_cancel.cancelled() => break,
                result = inbound_rx.recv() => match result {
                    Some(text) => text,
                    None => break,
                },
            };
            match handle_idle_socket_input(
                text,
                &mut current_session_id,
                &current_session_ref,
                connection_id,
                &state,
                &tx,
                &live_tx,
                &cancel,
                &stop_requested,
            )
            .await
            {
                IdleSocketInputAction::Continue => continue,
                IdleSocketInputAction::StartAgent {
                    run_mode,
                    reservation,
                    model_snapshot,
                } => {
                    agent_run_mode = run_mode;
                    agent_run_reservation = Some(reservation);
                    agent_model_snapshot = Some(model_snapshot);
                }
                IdleSocketInputAction::SwitchSession { session_id, result } => {
                    match switch_socket_session(
                        &state,
                        &tx,
                        &current_session_ref,
                        &mut current_session_id,
                        &connection_cancel,
                        connection_id,
                        session_id,
                    )
                    .await
                    {
                        Ok(()) => {
                            if result.session_list_changed {
                                broadcast_session_list_payload(&state).await;
                            }
                            ws_send(
                                &tx,
                                &json!({
                                    "type": result.response_type,
                                    "content": result.response,
                                    "dismissible": result.dismissible,
                                }),
                            )
                            .await;
                            continue;
                        }
                        Err(error) => {
                            ws_send(
                                &tx,
                                &json!({
                                    "type": "error",
                                    "content": error,
                                    "dismissible": true,
                                }),
                            )
                            .await;
                            continue;
                        }
                    }
                }
                IdleSocketInputAction::Break => break,
            }
        } // end if !rerun_agent
        run_active.store(true, Ordering::Relaxed);
        stop_requested.store(false, Ordering::Relaxed);

        let outcome = run_agent_session(
            &state,
            &current_session_id,
            connection_id,
            &cancel,
            &live_tx,
            &mut inbound_rx,
            &stop_requested,
            agent_run_mode,
            agent_run_reservation.take(),
            agent_model_snapshot.take(),
        )
        .await;
        run_active.store(false, Ordering::Relaxed);
        stop_requested.store(false, Ordering::Relaxed);
        rerun_agent = outcome.rerun_agent;

        if outcome.shutting_down {
            break;
        }
    }

    finalize_connection(
        &state,
        &current_session_id,
        connection_id,
        &connection_cancel,
        ConnectionCleanup {
            tx,
            live_tx,
            tasks: socket_tasks,
            reader,
            writer,
        },
    )
    .await;
}

#[derive(Deserialize)]
struct GroupSocketMessage {
    #[serde(rename = "type")]
    #[serde(default)]
    message_type: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    targets: Vec<String>,
    #[serde(default)]
    target_mode: Option<String>,
    #[serde(default)]
    start_runs: Option<bool>,
    #[serde(default)]
    run_mode: Option<String>,
}

async fn handle_group_socket(
    socket: WebSocket,
    state: Arc<AppState>,
    requested_group_id: String,
    requested_session_id: Option<String>,
) {
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

    if !group_socket_requested_session_is_main(requested_session_id.as_deref()) {
        let _ = ws_send(
            &tx,
            &json!({
                "type":"error",
                "content":"Group chat sockets require the UI connection query parameter session=main.",
                "dismissible":true,
            }),
        )
        .await;
        drop(tx);
        let _ = writer.await;
        return;
    }

    let group_id = match session_group::validate_group_id(&requested_group_id) {
        Ok(group_id) => group_id.to_string(),
        Err(error) => {
            let _ = ws_send(
                &tx,
                &json!({"type":"error","content":error,"dismissible":true}),
            )
            .await;
            drop(tx);
            let _ = writer.await;
            return;
        }
    };
    let group = {
        let gate = session_group::group_persist_gate(&group_id);
        let guard = gate.lock().await;
        let group = match session_group::load_group_from_storage_result(&group_id) {
            Ok(group) => group,
            Err(_) => {
                drop(guard);
                send_storage_status(&tx, &state).await;
                drop(tx);
                let _ = writer.await;
                return;
            }
        };
        if group.is_some() {
            let mut clients = state.group_clients.lock().await;
            // Cancel any prior connection bound to this group before replacing it, so a
            // displaced socket (a second tab, or a reconnect racing teardown) is told to
            // stop instead of lingering as a half-dead listener.
            if let Some(previous) = clients.insert(
                group_id.clone(),
                GroupClientBinding {
                    connection_id,
                    tx: tx.clone(),
                    cancel: connection_cancel.clone(),
                },
            ) {
                previous.cancel.cancel();
            }
        }
        group
    };
    let Some(group) = group else {
        let _ = ws_send(
            &tx,
            &json!({
                "type":"error",
                "content": format!("Group '{}' not found", group_id),
                "dismissible": true,
            }),
        )
        .await;
        drop(tx);
        let _ = writer.await;
        return;
    };
    let group_info = session_control::group_info_json(&state, &group).await;
    ws_send(&tx, &group_info).await;
    let group_history = session_control::group_history_json(&state, &group).await;
    ws_send(&tx, &group_history).await;
    for run in group
        .runs
        .iter()
        .filter(|run| run.status.as_str() == "running")
    {
        replay_group_member_live_round(&tx, &state, &group_id, run).await;
    }
    if let Ok(group_list) = build_group_list_payload() {
        ws_send(&tx, &group_list).await;
    }
    if let Ok(session_list) = socket_sync::build_session_list_payload(&state) {
        ws_send(&tx, &session_list).await;
    }
    send_storage_status(&tx, &state).await;

    loop {
        let result = tokio::select! {
            biased;
            _ = connection_cancel.cancelled() => break,
            result = rx.next() => result,
        };
        let text = match result {
            Some(Ok(WsMsg::Text(text))) => text.to_string(),
            Some(Ok(WsMsg::Close(_))) | Some(Err(_)) | None => break,
            _ => continue,
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !state.storage_is_writable() {
            send_storage_status(&tx, &state).await;
            continue;
        }
        let payload = if trimmed.starts_with('{') {
            match serde_json::from_str::<GroupSocketMessage>(trimmed) {
                Ok(payload) => payload,
                Err(error) => {
                    ws_send(
                        &tx,
                        &json!({
                            "type":"system",
                            "content": format!("Invalid group message JSON: {error}"),
                            "dismissible": true,
                        }),
                    )
                    .await;
                    continue;
                }
            }
        } else {
            if trimmed.starts_with('/') {
                ws_send(
                    &tx,
                    &json!({
                        "type":"system",
                        "content":"Slash commands are not supported in group chat.",
                        "dismissible": true,
                    }),
                )
                .await;
                continue;
            }
            GroupSocketMessage {
                message_type: Some("group_message".to_string()),
                text: Some(trimmed.to_string()),
                targets: Vec::new(),
                target_mode: Some("all".to_string()),
                start_runs: Some(true),
                run_mode: Some("execute".to_string()),
            }
        };
        let message_type = payload.message_type.as_deref().unwrap_or("group_message");
        if message_type == "group_stop" {
            match session_control::handle_group_socket_stop(&state, &group_id, payload.targets)
                .await
            {
                Ok(content) => {
                    ws_send(
                        &tx,
                        &json!({"type":"system","content":content,"dismissible":true}),
                    )
                    .await;
                }
                Err(error) => {
                    if state.storage_is_writable() {
                        ws_send(
                            &tx,
                            &json!({"type":"error","content":error,"dismissible":true}),
                        )
                        .await;
                    } else {
                        eprintln!("ERROR: group stop entered storage protection: {error}");
                        send_storage_status(&tx, &state).await;
                    }
                }
            }
            continue;
        }
        if message_type != "group_message" {
            ws_send(
                &tx,
                &json!({
                    "type":"system",
                    "content":"Unsupported group socket message type.",
                    "dismissible": true,
                }),
            )
            .await;
            continue;
        }
        if let Err(error) = session_control::handle_group_socket_message(
            &state,
            &group_id,
            session_control::GroupSocketDispatch {
                text: payload.text.unwrap_or_default(),
                targets: payload.targets,
                target_mode: payload.target_mode.unwrap_or_else(|| "all".to_string()),
                start_runs: payload.start_runs.unwrap_or(true),
                run_mode: payload.run_mode.unwrap_or_else(|| "execute".to_string()),
            },
        )
        .await
        {
            if state.storage_is_writable() {
                let event = if error == "group_plan_mode_unsupported" {
                    json!({
                        "type":"error",
                        "code":"group_plan_mode_unsupported",
                        "content":"Plan Mode is not available in Group chats. Open a Session to create and approve a plan.",
                        "dismissible":true,
                    })
                } else {
                    json!({"type":"error","content":error,"dismissible":true})
                };
                ws_send(&tx, &event).await;
            } else {
                eprintln!("ERROR: group message entered storage protection: {error}");
                send_storage_status(&tx, &state).await;
            }
        }
    }

    {
        let mut clients = state.group_clients.lock().await;
        if clients.get(&group_id).map(|binding| binding.connection_id) == Some(connection_id) {
            clients.remove(&group_id);
        }
    }
    drop(tx);
    let _ = writer.await;
}

fn group_socket_requested_session_is_main(requested_session_id: Option<&str>) -> bool {
    requested_session_id.map(str::trim).is_some_and(is_main)
}

async fn switch_socket_session(
    state: &AppState,
    tx: &WsTx,
    current_session_ref: &Arc<Mutex<String>>,
    current_session_id: &mut String,
    connection_cancel: &CancellationToken,
    connection_id: u64,
    next_session_id: String,
) -> Result<(), String> {
    if next_session_id == *current_session_id {
        return Ok(());
    }

    let previous_session_id = current_session_id.clone();
    session_store::save_current_session_to_disk(state, &previous_session_id)
        .await
        .map_err(|err| {
            format!("Failed to save session '{previous_session_id}' before switch: {err}")
        })?;

    unbind_session_connection_if_matches(state, &previous_session_id, connection_id).await;
    {
        let mut cancels = state.connection_cancels.lock().await;
        cancels.remove(&previous_session_id);
    }
    replace_connection_cancel_binding(state, &next_session_id, connection_id, connection_cancel)
        .await;

    *current_session_id = next_session_id.clone();
    {
        let mut guard = current_session_ref.lock().await;
        *guard = next_session_id.clone();
    }

    bind_session_connection(state, &next_session_id, connection_id, tx, false).await;
    send_existing_session_payloads(tx, state, &next_session_id).await;
    replay_live_round(tx, state, &next_session_id).await;
    finish_session_replay(state, &next_session_id, connection_id).await;
    Ok(())
}

// ── HTTP API ──────────────────────────────────────────────────────────────────

fn storage_status_payload(state: &AppState) -> serde_json::Value {
    let status = state.storage_status();
    match status.mode {
        storage::StorageMode::Healthy => json!({ "mode": "healthy" }),
        storage::StorageMode::Protected => json!({
            "mode": "protected",
            "code": "storage_protected",
        }),
    }
}

fn should_continue_after_default_session_error(status: &storage::StorageStatus) -> bool {
    status.mode == storage::StorageMode::Protected
}

pub(crate) fn storage_protected_api_error() -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": "Local storage is in protected mode. Repair it and restart LingClaw.",
            "code": "storage_protected",
        })),
    )
}

fn is_storage_mutation(method: &Method, path: &str) -> bool {
    matches!(
        (method, path),
        (
            &Method::POST | &Method::PUT | &Method::DELETE,
            "/api/session"
        ) | (
            &Method::POST | &Method::PUT | &Method::DELETE,
            "/api/session-group"
        ) | (&Method::PUT | &Method::DELETE, "/api/session-group/member")
            | (&Method::PUT, "/api/session-skills")
            | (&Method::PUT, "/api/mcp/session-policy")
            | (&Method::PUT, "/api/todos")
            | (&Method::POST, "/api/upload-images")
    )
}

#[derive(Clone)]
struct UploadedImageCleanup {
    s3_cfg: config::S3Config,
    object_keys: Vec<String>,
}

async fn enforce_storage_writable(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let storage_mutation = is_storage_mutation(request.method(), request.uri().path());
    let external_upload =
        request.method() == Method::POST && request.uri().path() == "/api/upload-images";
    if storage_mutation && !state.storage_is_writable() {
        return storage_protected_api_error().into_response();
    }
    let response = next.run(request).await;
    if should_replace_with_storage_protected(
        storage_mutation,
        external_upload,
        response.status(),
        state.storage_is_writable(),
    ) {
        if external_upload
            && let Some(cleanup) = response.extensions().get::<UploadedImageCleanup>()
        {
            schedule_uploaded_image_cleanup(&state, &cleanup.s3_cfg, cleanup.object_keys.clone());
        }
        return storage_protected_api_error().into_response();
    }
    response
}

fn should_replace_with_storage_protected(
    storage_mutation: bool,
    external_upload: bool,
    response_status: StatusCode,
    storage_writable: bool,
) -> bool {
    storage_mutation && !storage_writable && (external_upload || !response_status.is_success())
}

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
    let config = state.config();
    Json(json!({
        "status": "ok",
        "version": VERSION,
        "model": config.model,
        "sessions": sessions.len(),
        "storage": storage_status_payload(&state),
    }))
}

async fn api_sessions(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let persisted_summaries =
        crate::session_store::list_saved_session_summaries_result(&sessions_dir())
            .map_err(|_| storage_protected_api_error())?;
    let config = state.config();
    let mut list: Vec<serde_json::Value> = Vec::new();
    let mut seen_ids = HashSet::new();

    let mut summaries = persisted_summaries;
    {
        let sessions = state.sessions.lock().await;
        for session in sessions.values() {
            if let Some(summary) = summaries
                .iter_mut()
                .find(|summary| session_ids_match(&summary.id, &session.id))
            {
                *summary = SessionSummary::from_session(session);
            } else {
                summaries.push(SessionSummary::from_session(session));
            }
        }
    }

    for summary in summaries {
        let dedupe_id = if cfg!(windows) {
            summary.id.to_ascii_lowercase()
        } else {
            summary.id.clone()
        };
        if !seen_ids.insert(dedupe_id) {
            continue;
        }
        list.push(summary.to_json(&config, None));
    }

    sort_session_json_values(&mut list);
    Ok(Json(json!({"sessions": list})))
}

fn sort_session_json_values(list: &mut [serde_json::Value]) {
    list.sort_by(|a, b| {
        let a_id = a["id"].as_str().unwrap_or_default();
        let b_id = b["id"].as_str().unwrap_or_default();
        match (is_main(a_id), is_main(b_id)) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => {
                let b_ts = b["updated_at"].as_u64().unwrap_or(0);
                let a_ts = a["updated_at"].as_u64().unwrap_or(0);
                b_ts.cmp(&a_ts).then_with(|| a_id.cmp(b_id))
            }
        }
    });
}

fn validate_session_display_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Session name cannot be empty.".to_string());
    }
    if trimmed.chars().count() > 80 {
        return Err("Session name must be 80 characters or fewer.".to_string());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("Session name cannot contain control characters.".to_string());
    }
    Ok(trimmed.to_string())
}

async fn generate_available_session_id(
    state: &AppState,
) -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    for _ in 0..128 {
        let id = generate_random_session_id().map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": error })),
            )
        })?;
        if crate::session_store::validate_session_id(&id).is_err() {
            continue;
        }
        {
            let sessions = state.sessions.lock().await;
            if find_loaded_session_id(&sessions, &id).is_some() {
                continue;
            }
        }
        if crate::session_store::canonical_saved_session_id_result(&id)
            .map_err(|_| storage_protected_api_error())?
            .is_some()
        {
            continue;
        }
        return Ok(id);
    }
    Err((
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": "Failed to generate a unique session id" })),
    ))
}

async fn api_post_session(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

    let session_id = generate_available_session_id(&state).await?;
    let persist_gate = session_persist_gate(&session_id);
    let _persist_guard = persist_gate.lock().await;

    {
        let sessions = state.sessions.lock().await;
        if find_loaded_session_id(&sessions, &session_id).is_some() {
            return Err((
                StatusCode::CONFLICT,
                Json(json!({ "error": "Generated session id already exists" })),
            ));
        }
    }
    if crate::session_store::canonical_saved_session_id_result(&session_id)
        .map_err(|_| storage_protected_api_error())?
        .is_some()
    {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "Generated session id already exists" })),
        ));
    }

    let config = state.config();
    let session_name = format!("Session {session_id}");
    if !state.storage_is_writable() {
        return Err(storage_protected_api_error());
    }
    let cleanup_fresh_workspace = !session_workspace_path(&session_id).exists();
    let mut session = Session::new_with_id(&session_id, &session_name);
    let model = session.effective_model(&config.model).to_string();
    let sys = prompts::build_system_prompt(
        &config,
        &session.workspace,
        &model,
        &session.enabled_system_skills,
    );
    session.messages.push(sys);

    if let Err(error) = save_session_to_disk_locked(&session).await {
        if cleanup_fresh_workspace {
            session_control::cleanup_failed_created_session(&session_id, &session.workspace);
        }
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to save new session: {error}") })),
        ));
    }

    let payload = {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session.clone());
        json!({
            "ok": true,
            "session": SessionSummary::from_session(&session).to_json(&config, Some(&session)),
        })
    };

    broadcast_session_list_payload(&state).await;
    Ok(Json(payload))
}

async fn api_put_session(
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(request): Json<SessionRenameRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

    let requested_session_id = query.session.as_deref().unwrap_or(MAIN_SESSION_ID);
    let session_id = ensure_session_loaded_for_api(&state, requested_session_id).await?;
    let name = validate_session_display_name(&request.name)
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;

    let model_status_guard = CONFIG_FILE_LOCK.read().await;
    let persist_gate = session_persist_gate(&session_id);
    let _persist_guard = persist_gate.lock().await;

    let (session_to_save, old_session, payload, session_event) = {
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(&session_id) else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Session '{}' not found", session_id) })),
            ));
        };
        let old_session = session.clone();
        session.name = name.clone();
        session.updated_at = now_epoch();

        let (config, config_revision) = state.config_snapshot_with_revision();
        let (model, model_override_configured, effective_model_configured) =
            session.model_configuration(&config);
        let usage = socket_sync::build_session_usage_payload(session);
        let session_event = socket_sync::build_session_info_payload(
            &session_id,
            &session.name,
            &config,
            &model,
            session.model_override.is_some(),
            model_override_configured,
            effective_model_configured,
            config_revision,
            usage,
        );
        let payload = json!({
            "ok": true,
            "session": SessionSummary::from_session(session).to_json(&config, Some(session)),
        });

        (session.clone(), old_session, payload, session_event)
    };

    if let Err(error) = save_session_to_disk_locked(&session_to_save).await {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(&session_id) {
            *session = old_session;
        }
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to save session name: {error}") })),
        ));
    }

    drop(model_status_guard);
    send_session_client_event(&state, &session_id, session_event).await;
    broadcast_session_list_payload(&state).await;

    Ok(Json(payload))
}

fn build_group_list_payload() -> Result<serde_json::Value, String> {
    let groups = session_group::list_saved_group_summaries_result()?
        .into_iter()
        .map(|summary| summary.to_json())
        .collect::<Vec<_>>();
    Ok(json!({"type":"session_group_list","groups": groups}))
}

pub(crate) async fn broadcast_group_list_payload(state: &AppState) {
    let payload = match build_group_list_payload() {
        Ok(payload) => payload,
        Err(_) => {
            #[cfg(not(test))]
            broadcast_storage_status(state).await;
            return;
        }
    };
    let session_ids = {
        let clients = state.session_clients.lock().await;
        clients.keys().cloned().collect::<Vec<_>>()
    };
    for session_id in session_ids {
        send_session_client_event(state, &session_id, payload.clone()).await;
    }
    let group_ids = {
        let clients = state.group_clients.lock().await;
        clients.keys().cloned().collect::<Vec<_>>()
    };
    for group_id in group_ids {
        send_group_client_event(
            state,
            &group_id,
            session_control::GroupClientEvent::reliable(payload.clone()),
        )
        .await;
    }
}

fn find_loaded_session_id(sessions: &HashMap<String, Session>, session_id: &str) -> Option<String> {
    sessions
        .keys()
        .find(|existing_id| session_ids_match(existing_id, session_id))
        .cloned()
}

async fn ensure_session_loaded_for_api(
    state: &AppState,
    requested_session_id: &str,
) -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    let requested_session_id = crate::session_store::validate_session_id(requested_session_id)
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?
        .to_string();

    {
        let sessions = state.sessions.lock().await;
        if let Some(session_id) = find_loaded_session_id(&sessions, &requested_session_id) {
            return Ok(session_id);
        }
    }

    if let Some(session) =
        crate::session_store::load_session_from_storage_result(&requested_session_id)
            .map_err(|_| storage_protected_api_error())?
    {
        let session_id = session.id.clone();
        let mut sessions = state.sessions.lock().await;
        let effective_id = find_loaded_session_id(&sessions, &session_id).unwrap_or(session_id);
        sessions.entry(effective_id.clone()).or_insert(session);
        return Ok(effective_id);
    }

    Err((
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("Session '{}' not found", requested_session_id) })),
    ))
}

fn build_session_skill_payloads(
    workspace: &Path,
    enabled_system_skills: &HashSet<String>,
) -> Vec<serde_json::Value> {
    prompts::discover_skills_by_source(workspace, prompts::SkillSource::System)
        .into_iter()
        .filter_map(|skill| {
            let id = prompts::system_skill_relative_dir(&skill.path)?;
            let group = id.split('/').next().unwrap_or(id.as_str()).to_string();
            let enabled = prompts::is_system_skill_enabled(&skill.path, enabled_system_skills);
            Some(json!({
                "id": id,
                "name": skill.name,
                "description": skill.description,
                "path": skill.path,
                "group": group,
                "enabled": enabled,
            }))
        })
        .collect()
}

fn session_system_skill_status_lists(
    all_skill_ids: &HashSet<String>,
    enabled_system_skills: &HashSet<String>,
) -> (Vec<String>, Vec<String>) {
    let mut enabled = Vec::new();
    let mut disabled = Vec::new();
    for id in all_skill_ids {
        let path = format!("system://skills/{id}/SKILL.md");
        if prompts::is_system_skill_enabled(&path, enabled_system_skills) {
            enabled.push(id.clone());
        } else {
            disabled.push(id.clone());
        }
    }
    enabled.sort();
    disabled.sort();
    (enabled, disabled)
}

async fn api_session_skills(
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

    let requested_session_id = query.session.as_deref().unwrap_or(MAIN_SESSION_ID);
    let session_id = ensure_session_loaded_for_api(&state, requested_session_id).await?;
    let (session_name, workspace, enabled_system_skills) = {
        let sessions = state.sessions.lock().await;
        let Some(session) = sessions.get(&session_id) else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Session '{}' not found", session_id) })),
            ));
        };
        (
            session.name.clone(),
            session.workspace.clone(),
            session.enabled_system_skills.clone(),
        )
    };

    let all_skill_ids =
        prompts::discover_skills_by_source(&workspace, prompts::SkillSource::System)
            .iter()
            .filter_map(|skill| prompts::system_skill_relative_dir(&skill.path))
            .collect::<HashSet<_>>();
    let (enabled, disabled) =
        session_system_skill_status_lists(&all_skill_ids, &enabled_system_skills);

    Ok(Json(json!({
        "session": {
            "id": session_id,
            "name": session_name,
        },
        "skills": build_session_skill_payloads(&workspace, &enabled_system_skills),
        "enabledSystemSkills": enabled,
        "disabledSystemSkills": disabled,
    })))
}

fn normalize_enabled_system_skill_id(id: &str) -> Result<String, String> {
    let trimmed = id.trim().trim_matches('/');
    if trimmed.is_empty()
        || trimmed.contains("..")
        || trimmed.split('/').any(str::is_empty)
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/'))
    {
        return Err(format!("Invalid system skill id: {id}"));
    }
    Ok(trimmed.to_string())
}

async fn api_put_session_skills(
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(request): Json<SessionSkillsUpdateRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

    let requested_session_id = query.session.as_deref().unwrap_or(MAIN_SESSION_ID);
    let session_id = ensure_session_loaded_for_api(&state, requested_session_id).await?;
    let persist_gate = session_persist_gate(&session_id);
    let _persist_guard = persist_gate.lock().await;

    let (workspace, all_skill_ids, current_enabled_system_skills) = {
        let sessions = state.sessions.lock().await;
        let Some(session) = sessions.get(&session_id) else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Session '{}' not found", session_id) })),
            ));
        };
        let workspace = session.workspace.clone();
        let all_skill_ids =
            prompts::discover_skills_by_source(&workspace, prompts::SkillSource::System)
                .iter()
                .filter_map(|skill| prompts::system_skill_relative_dir(&skill.path))
                .collect::<HashSet<_>>();
        (
            workspace,
            all_skill_ids,
            session.enabled_system_skills.clone(),
        )
    };

    let managed_skill_ids = match &request.known_system_skills {
        Some(raw_ids) => {
            let mut ids = HashSet::new();
            for raw_id in raw_ids {
                let id = normalize_enabled_system_skill_id(raw_id)
                    .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
                if !all_skill_ids.contains(&id) {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        Json(json!({ "error": format!("Unknown system skill id: {id}") })),
                    ));
                }
                ids.insert(id);
            }
            ids
        }
        None => all_skill_ids.clone(),
    };

    let mut enabled = HashSet::new();
    for raw_id in &request.enabled_system_skills {
        let id = normalize_enabled_system_skill_id(raw_id)
            .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
        if !all_skill_ids.contains(&id) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Unknown system skill id: {id}") })),
            ));
        }
        if !managed_skill_ids.contains(&id) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(
                    json!({ "error": format!("Enabled system skill id was not loaded by client: {id}") }),
                ),
            ));
        }
        enabled.insert(id);
    }

    let enabled_system_skills = all_skill_ids
        .iter()
        .filter(|id| {
            if managed_skill_ids.contains(*id) {
                enabled.contains(*id)
            } else {
                let path = format!("system://skills/{id}/SKILL.md");
                prompts::is_system_skill_enabled(&path, &current_enabled_system_skills)
            }
        })
        .cloned()
        .collect::<HashSet<_>>();

    let (session_to_save, response_payload) = {
        let mut sessions = state.sessions.lock().await;
        let Some(session) = sessions.get_mut(&session_id) else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Session '{}' not found", session_id) })),
            ));
        };
        let old_session = session.clone();
        session.enabled_system_skills = enabled_system_skills.clone();
        session.updated_at = now_epoch();
        refresh_session_system_prompt(&state, session);

        let skills = build_session_skill_payloads(&workspace, &session.enabled_system_skills);
        let (enabled, disabled) =
            session_system_skill_status_lists(&all_skill_ids, &session.enabled_system_skills);
        let payload = json!({
            "ok": true,
            "session": {
                "id": session.id,
                "name": session.name,
            },
            "skills": skills,
            "enabledSystemSkills": enabled,
            "disabledSystemSkills": disabled,
        });

        (session.clone(), (old_session, payload))
    };

    let (old_session, payload) = response_payload;
    if let Err(error) = save_session_to_disk_locked(&session_to_save).await {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(&session_id) {
            *session = old_session;
        }
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to save session skills: {error}") })),
        ));
    }

    Ok(Json(payload))
}

async fn api_mcp_catalog(
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

    let requested_session_id = query.session.as_deref().unwrap_or(MAIN_SESSION_ID);
    let session_id = ensure_session_loaded_for_api(&state, requested_session_id).await?;
    let (session_name, workspace) = {
        let sessions = state.sessions.lock().await;
        let Some(session) = sessions.get(&session_id) else {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Session '{}' not found", session_id) })),
            ));
        };
        (session.name.clone(), session.workspace.clone())
    };

    let config = state.config();
    let policy = tools::mcp::load_session_policy(&workspace);
    let catalog = tools::mcp::catalog_snapshot(&config, &workspace).await;
    let auth_state = tools::mcp::load_auth_state();

    let server_reports = catalog
        .reports
        .iter()
        .map(|report| (report.server_name.as_str(), report))
        .collect::<HashMap<_, _>>();
    let mut servers = config
        .mcp_servers
        .iter()
        .map(|(name, server)| {
            let report = server_reports.get(name.as_str());
            json!({
                "id": name,
                "name": name,
                "transport": server.effective_transport(),
                "configuredEnabled": server.enabled,
                "enabled": policy.enabled_servers.contains(name),
                "authenticated": auth_state
                    .servers
                    .get(name)
                    .is_some_and(|auth| tools::mcp::auth_state_usable_for_server(name, server, auth)),
                "toolCount": report.map(|r| r.tool_names.len()).unwrap_or(0),
                "resourceCount": report.map(|r| r.resource_count).unwrap_or(0),
                "promptCount": report.map(|r| r.prompt_count).unwrap_or(0),
                "error": report.and_then(|r| r.error.clone()),
            })
        })
        .collect::<Vec<_>>();
    servers.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["name"].as_str().unwrap_or_default())
    });

    let mut tools_payload = catalog
        .tools
        .iter()
        .map(|tool| {
            json!({
                "id": tool.exposed_name,
                "server": tool.server_name,
                "rawName": tool.raw_name,
                "name": tool.exposed_name,
                "description": tool.description,
                "readOnly": tools::mcp::is_read_only_tool_descriptor(tool),
                "enabled": policy.allows_tool(tool),
            })
        })
        .collect::<Vec<_>>();
    tools_payload.sort_by(|a, b| {
        a["id"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["id"].as_str().unwrap_or_default())
    });

    let mut resources_payload = catalog
        .resources
        .iter()
        .map(|resource| {
            json!({
                "server": resource.server_name,
                "uri": resource.uri,
                "name": resource.name,
                "description": resource.description,
                "mimeType": resource.mime_type,
            })
        })
        .collect::<Vec<_>>();
    resources_payload.sort_by(|a, b| {
        a["uri"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["uri"].as_str().unwrap_or_default())
    });

    let mut prompts_payload = catalog
        .prompts
        .iter()
        .map(|prompt| {
            json!({
                "server": prompt.server_name,
                "name": prompt.raw_name,
                "description": prompt.description,
                "arguments": prompt.arguments,
            })
        })
        .collect::<Vec<_>>();
    prompts_payload.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["name"].as_str().unwrap_or_default())
    });

    Ok(Json(json!({
        "session": {
            "id": session_id,
            "name": session_name,
        },
        "policy": policy,
        "servers": servers,
        "tools": tools_payload,
        "resources": resources_payload,
        "prompts": prompts_payload,
    })))
}

async fn api_put_mcp_session_policy(
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(request): Json<McpSessionPolicyUpdateRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

    let requested_session_id = query.session.as_deref().unwrap_or(MAIN_SESSION_ID);
    let session_id = ensure_session_loaded_for_api(&state, requested_session_id).await?;
    let workspace = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .map(|session| session.workspace.clone())
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": format!("Session '{}' not found", session_id) })),
                )
            })?
    };

    let config = state.config();
    let previous_policy = tools::mcp::load_session_policy(&workspace);
    let known_servers = config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, _)| name.clone())
        .collect::<HashSet<_>>();

    let mut enabled_servers = HashSet::new();
    for server in request.enabled_servers {
        if !known_servers.contains(&server) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Unknown or disabled MCP server: {server}") })),
            ));
        }
        enabled_servers.insert(server);
    }

    let requested_tools = request.enabled_tools;
    let mut servers_to_probe = HashSet::new();
    for tool in &requested_tools {
        for server_name in &enabled_servers {
            if tools::mcp::exposed_tool_matches_server(tool, server_name) {
                servers_to_probe.insert(server_name.clone());
            }
        }
    }

    let (known_tools, successful_tool_servers) = if servers_to_probe.is_empty() {
        (HashMap::new(), HashSet::new())
    } else {
        let (tools, successful_servers) = tools::mcp::list_tools_for_servers_uncached_with_status(
            &config,
            &workspace,
            &servers_to_probe,
        )
        .await;
        (
            tools
                .into_iter()
                .map(|tool| (tool.exposed_name.clone(), tool.server_name.clone()))
                .collect::<HashMap<_, _>>(),
            successful_servers,
        )
    };

    let mut enabled_tools = HashSet::new();
    for tool in requested_tools {
        if let Some(server_name) = known_tools.get(&tool) {
            if !enabled_servers.contains(server_name) {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(
                        json!({ "error": format!("MCP tool '{tool}' belongs to disabled server '{server_name}'") }),
                    ),
                ));
            }
            enabled_tools.insert(tool);
            continue;
        }

        let previous_matching_servers = previous_policy
            .enabled_servers
            .iter()
            .filter(|server_name| {
                enabled_servers.contains(*server_name)
                    && tools::mcp::exposed_tool_matches_server(&tool, server_name)
            })
            .collect::<Vec<_>>();
        let was_previously_enabled_for_server =
            previous_policy.enabled_tools.contains(&tool) && !previous_matching_servers.is_empty();
        let matching_server_successfully_probed = previous_matching_servers
            .iter()
            .any(|server_name| successful_tool_servers.contains(*server_name));
        if was_previously_enabled_for_server && !matching_server_successfully_probed {
            enabled_tools.insert(tool);
            continue;
        }

        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Unknown MCP tool: {tool}") })),
        ));
    }

    let policy = tools::mcp::McpSessionPolicy {
        enabled_servers,
        enabled_tools,
        confirm_mutating_tools: request.confirm_mutating_tools,
        client_capabilities: request.client_capabilities,
    };
    tools::mcp::save_session_policy(&workspace, &policy).map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error })),
        )
    })?;
    Ok(Json(json!({ "ok": true, "policy": policy })))
}

fn mcp_oauth_timeout_secs(config: &Config, server_name: &str) -> u64 {
    config
        .mcp_servers
        .get(server_name)
        .and_then(|server| server.timeout_secs)
        .unwrap_or(config.tool_timeout.as_secs())
        .max(1)
}

async fn api_mcp_auth_start(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(request): Json<McpServerRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
    let config = state.config();
    let Some(server) = config.mcp_servers.get(&request.server) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Unknown MCP server: {}", request.server) })),
        ));
    };
    if !server.enabled {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("MCP server '{}' is disabled", request.server) })),
        ));
    }
    if server.effective_transport() != "streamable-http" {
        return Ok(Json(json!({
            "ok": false,
            "error": "OAuth is only used for streamable-http MCP servers"
        })));
    }
    let timeout_secs = mcp_oauth_timeout_secs(&config, &request.server);
    let started =
        tools::mcp::start_oauth_authorization(&request.server, server, config.port, timeout_secs)
            .await
            .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
    Ok(Json(json!({
        "ok": true,
        "server": started.server,
        "authorizationUrl": started.authorization_url,
        "redirectUri": started.redirect_uri,
        "clientId": started.client_id,
        "scopes": started.scopes,
    })))
}

async fn api_mcp_auth_callback_post(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(request): Json<McpAuthCallbackRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
    let config = state.config();
    let timeout_secs = mcp_oauth_timeout_secs(&config, &request.server);
    let completed = tools::mcp::complete_oauth_authorization(
        &request.server,
        &request.code,
        &request.state,
        timeout_secs,
    )
    .await
    .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
    Ok(Json(json!({
        "ok": true,
        "server": request.server,
        "expiresAt": completed.expires_at,
        "scopes": completed.scopes,
    })))
}

async fn api_mcp_auth_callback_get(
    Query(query): Query<McpAuthCallbackQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    // OAuth authorization redirects often include an external Referer from the
    // authorization server. The loopback Host plus OAuth state check is the
    // boundary for this callback.
    validate_loopback_host_header(&headers)?;
    if let Some(error) = query.error {
        let description = query.error_description.unwrap_or_default();
        return Ok((
            StatusCode::BAD_REQUEST,
            format!("MCP OAuth failed: {error} {description}"),
        )
            .into_response());
    }
    let code = query.code.as_deref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing code" })),
        )
    })?;
    let oauth_state = query.state.as_deref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing state" })),
        )
    })?;
    let config = state.config();
    let timeout_secs = mcp_oauth_timeout_secs(&config, &query.server);
    tools::mcp::complete_oauth_authorization(&query.server, code, oauth_state, timeout_secs)
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
    Ok("MCP OAuth authorization completed. You can close this window.".into_response())
}

async fn api_mcp_auth_disconnect(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(request): Json<McpServerRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
    let config = state.config();
    if let Some(server) = config.mcp_servers.get(&request.server) {
        tools::mcp::terminate_http_sessions_for_server(&request.server, server).await;
    }
    let mut auth = tools::mcp::load_auth_state();
    auth.servers.remove(&request.server);
    tools::mcp::save_auth_state(&auth).map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error })),
        )
    })?;
    tools::mcp::clear_cached_runtime_state_for_server(&request.server);
    Ok(Json(json!({ "ok": true })))
}

async fn api_mcp_resource_read(
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(request): Json<McpResourceReadRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
    let requested_session_id = query.session.as_deref().unwrap_or(MAIN_SESSION_ID);
    let session_id = ensure_session_loaded_for_api(&state, requested_session_id).await?;
    let workspace = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .map(|session| session.workspace.clone())
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": format!("Session '{}' not found", session_id) })),
                )
            })?
    };
    let config = state.config();
    ensure_mcp_server_enabled_for_session(&config, &workspace, &request.server)?;
    let result = tools::mcp::read_resource(&request.server, &request.uri, &config, &workspace)
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
    Ok(Json(json!({ "ok": true, "result": result })))
}

async fn api_mcp_prompt_get(
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(request): Json<McpPromptGetRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
    let requested_session_id = query.session.as_deref().unwrap_or(MAIN_SESSION_ID);
    let session_id = ensure_session_loaded_for_api(&state, requested_session_id).await?;
    let workspace = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .map(|session| session.workspace.clone())
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": format!("Session '{}' not found", session_id) })),
                )
            })?
    };
    let config = state.config();
    ensure_mcp_server_enabled_for_session(&config, &workspace, &request.server)?;
    let result = tools::mcp::get_prompt(
        &request.server,
        &request.name,
        request.arguments,
        &config,
        &workspace,
    )
    .await
    .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
    Ok(Json(json!({ "ok": true, "result": result })))
}

fn ensure_mcp_server_enabled_for_session(
    config: &Config,
    workspace: &Path,
    server_name: &str,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    let Some(server) = config.mcp_servers.get(server_name) else {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("Unknown MCP server: {server_name}") })),
        ));
    };
    if !server.enabled {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("MCP server '{server_name}' is disabled") })),
        ));
    }
    let policy = tools::mcp::load_session_policy(workspace);
    if policy.enabled_servers.contains(server_name) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": format!("MCP server '{server_name}' is not enabled for this session")
            })),
        ))
    }
}

async fn api_todos(
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(request): Json<crate::todos::TodoReplaceRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

    let requested_session_id = query.session.as_deref().unwrap_or(MAIN_SESSION_ID);
    let session_id = ensure_session_loaded_for_api(&state, requested_session_id).await?;

    match crate::todos::replace_session_todos(
        state.as_ref(),
        &session_id,
        request,
        crate::todos::TodoUpdateOrigin::User,
    )
    .await
    {
        Ok(response) => {
            let status = if response.conflict {
                StatusCode::CONFLICT
            } else {
                StatusCode::OK
            };
            Ok((
                status,
                Json(serde_json::to_value(response).unwrap_or_else(|_| json!({}))),
            ))
        }
        Err(crate::todos::TodoUpdateError::SessionNotFound) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Session not found" })),
        )),
        Err(crate::todos::TodoUpdateError::Validation(error)) => {
            Err((StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))
        }
        Err(crate::todos::TodoUpdateError::Persist(error)) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error })),
        )),
    }
}

async fn api_client_config(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
    let config = state.config();
    Ok(Json(json!({
        "upload_token": state.upload_token,
        "s3_config_id": config.s3.as_ref().map(image_uploads::s3_config_id),
    })))
}

/// POST /api/upload-images — multipart image upload to S3.
async fn api_upload_images(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
    if !state.storage_is_writable() {
        return Err(storage_protected_api_error());
    }
    let upload_token = headers
        .get(UPLOAD_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "Missing upload token"})),
            )
        })?;
    if upload_token != state.upload_token {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": "Invalid upload token"})),
        ));
    }

    let config = state.config();
    let s3_cfg = config.s3.as_ref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "S3 not configured"})),
        )
    })?;
    let s3_config_id = image_uploads::s3_config_id(s3_cfg);

    let mut uploaded_images: Vec<serde_json::Value> = Vec::new();
    let mut uploaded_object_keys: Vec<String> = Vec::new();
    let mut urls: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let max_files = image_uploads::MAX_IMAGE_UPLOAD_FILES;

    loop {
        ensure_upload_storage_writable(&state, s3_cfg, &uploaded_object_keys)?;
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(e) => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": format!("Invalid multipart payload: {e}"),
                    })),
                ));
            }
        };

        if urls.len() + errors.len() >= max_files {
            errors.push("Maximum 10 images per upload".to_string());
            break;
        }

        let declared_content_type = field
            .content_type()
            .unwrap_or("application/octet-stream")
            .to_string();

        let data = match field.bytes().await {
            Ok(d) => d,
            Err(e) => {
                errors.push(format!("Read error: {e}"));
                continue;
            }
        };

        if data.len() > image_uploads::MAX_IMAGE_UPLOAD_BYTES {
            errors.push(format!(
                "Image too large ({} bytes, max {})",
                data.len(),
                image_uploads::MAX_IMAGE_UPLOAD_BYTES
            ));
            continue;
        }

        if data.is_empty() {
            errors.push("Empty image file".to_string());
            continue;
        }

        let Some(content_type) = image_uploads::detect_image_upload_content_type(&data) else {
            errors.push(format!(
                "Unsupported image content (declared type: {declared_content_type})"
            ));
            continue;
        };

        let object_key = image_uploads::generate_s3_object_key(s3_cfg, content_type, &data);
        ensure_upload_storage_writable(&state, s3_cfg, &uploaded_object_keys)?;

        let upload_timeout = std::time::Duration::from_secs(60);
        let upload_result = tokio::time::timeout(
            upload_timeout,
            image_uploads::s3_put_object(&state.http, s3_cfg, &object_key, &data, content_type),
        )
        .await;
        match upload_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                errors.push(e);
                continue;
            }
            Err(_) => {
                errors.push("S3 upload timed out".to_string());
                continue;
            }
        }
        uploaded_object_keys.push(object_key.clone());
        ensure_upload_storage_writable(&state, s3_cfg, &uploaded_object_keys)?;

        match image_uploads::s3_presigned_get_url(s3_cfg, &object_key) {
            Ok(url) => {
                let attachment_token =
                    image_uploads::sign_attachment_object_key(s3_cfg, &object_key);
                uploaded_images.push(json!({
                    "url": url.clone(),
                    "object_key": object_key,
                    "attachment_token": attachment_token,
                    "s3_config_id": s3_config_id.clone(),
                }));
                urls.push(url);
            }
            Err(e) => {
                schedule_uploaded_image_cleanup(&state, s3_cfg, vec![object_key.clone()]);
                uploaded_object_keys.retain(|key| key != &object_key);
                errors.push(e);
            }
        }
    }
    ensure_upload_storage_writable(&state, s3_cfg, &uploaded_object_keys)?;

    let mut response = Json(
        json!({ "images": uploaded_images, "urls": urls, "errors": errors, "s3_config_id": s3_config_id }),
    )
    .into_response();
    response.extensions_mut().insert(UploadedImageCleanup {
        s3_cfg: s3_cfg.clone(),
        object_keys: uploaded_object_keys,
    });
    Ok(response)
}

fn ensure_upload_storage_writable(
    state: &AppState,
    s3_cfg: &config::S3Config,
    uploaded_object_keys: &[String],
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if state.storage_is_writable() {
        return Ok(());
    }
    schedule_uploaded_image_cleanup(state, s3_cfg, uploaded_object_keys.to_vec());
    Err(storage_protected_api_error())
}

fn schedule_uploaded_image_cleanup(
    state: &AppState,
    s3_cfg: &config::S3Config,
    object_keys: Vec<String>,
) {
    if object_keys.is_empty() {
        return;
    }
    let http = state.http.clone();
    let s3_cfg = s3_cfg.clone();
    tokio::spawn(async move {
        for object_key in object_keys {
            let cleanup = tokio::time::timeout(
                Duration::from_secs(10),
                image_uploads::s3_delete_object(&http, &s3_cfg, &object_key),
            )
            .await;
            match cleanup {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    eprintln!("WARNING: failed to clean up an uncommitted image upload: {error}");
                }
                Err(_) => {
                    eprintln!("WARNING: timed out cleaning up an uncommitted image upload");
                }
            }
        }
    });
}

// ── Config & Usage API ───────────────────────────────────────────────────────

fn has_explicit_model_environment(value: Option<&str>) -> bool {
    value.is_some_and(|model| !model.trim().is_empty())
}

fn environment_model_configured() -> bool {
    let model = std::env::var("LINGCLAW_MODEL").ok();
    has_explicit_model_environment(model.as_deref())
}

fn configured_models_available(config: &Config) -> bool {
    config.providers.values().any(|provider| {
        provider
            .models
            .iter()
            .any(|model| !model.id.trim().is_empty())
    })
}

/// GET /api/config — read the raw JSON config file.
async fn api_get_config(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
    let workspace = session_workspace_path(MAIN_SESSION_ID);
    let discovered_agents: Vec<serde_json::Value> =
        subagents::discovery::discover_all_agents(&workspace)
            .into_iter()
            .map(|agent| {
                json!({
                    "name": agent.name,
                    "description": agent.description,
                    "source": agent.source.label(),
                })
            })
            .collect();
    let path = config_file_path().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Cannot determine config path"})),
        )
    })?;
    let (config, config_revision, content) = read_runtime_config_file_snapshot(&state, &path)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Cannot read config: {e}")})),
            )
        })?;
    let explicit_primary_model_configured = config.explicit_primary_model_configured;
    let configured_models_available = configured_models_available(&config);
    let config_file_etag = config_file_etag(&content);
    match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(value) => Ok(Json(json!({
            "config": value,
            "path": path.display().to_string(),
            "environmentModelConfigured": environment_model_configured(),
            "explicitPrimaryModelConfigured": explicit_primary_model_configured,
            "configuredModelsAvailable": configured_models_available,
            "configRevision": config_revision,
            "configFileEtag": config_file_etag,
            "discoveredAgents": discovered_agents.clone(),
        }))),
        Err(e) => {
            let msg = e.to_string();
            let (line, column) = parse_serde_error_position(&msg);
            Ok(Json(json!({
                "config": null,
                "raw": content,
                "path": path.display().to_string(),
                "environmentModelConfigured": environment_model_configured(),
                "explicitPrimaryModelConfigured": explicit_primary_model_configured,
                "configuredModelsAvailable": configured_models_available,
                "configRevision": config_revision,
                "configFileEtag": config_file_etag,
                "parse_error": msg,
                "line": line,
                "column": column,
                "discoveredAgents": discovered_agents,
            })))
        }
    }
}

fn s3_lifecycle_needs_sync_with_id(
    current: Option<&config::S3Config>,
    synced_config_id: Option<&str>,
) -> bool {
    let Some(current) = current.filter(|cfg| cfg.lifecycle_days > 0) else {
        return false;
    };
    let current_config_id = image_uploads::s3_config_id(current);
    synced_config_id != Some(current_config_id.as_str())
}

fn s3_lifecycle_needs_sync(current: Option<&config::S3Config>) -> bool {
    let synced_config_id = S3_LIFECYCLE_SYNCED_CONFIG_ID
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    s3_lifecycle_needs_sync_with_id(current, synced_config_id.as_deref())
}

fn s3_lifecycle_request_matches_current(
    current: Option<&config::S3Config>,
    requested_config_id: &str,
) -> bool {
    current
        .filter(|cfg| cfg.lifecycle_days > 0)
        .is_some_and(|cfg| image_uploads::s3_config_id(cfg) == requested_config_id)
}

async fn synchronize_s3_lifecycle(http: &Client, s3_cfg: &config::S3Config) {
    let config_id = image_uploads::s3_config_id(s3_cfg);

    let synchronized = match tokio::time::timeout(
        Duration::from_secs(30),
        image_uploads::ensure_s3_temp_image_lifecycle(http, s3_cfg),
    )
    .await
    {
        Ok(Ok(true)) => {
            eprintln!(
                "  S3 lifecycle: configured {}-day expiration for prefix '{}'",
                s3_cfg.lifecycle_days, s3_cfg.prefix
            );
            true
        }
        Ok(Ok(false)) => {
            eprintln!(
                "  S3 lifecycle: verified {}-day expiration for prefix '{}'",
                s3_cfg.lifecycle_days, s3_cfg.prefix
            );
            true
        }
        Ok(Err(error)) => {
            eprintln!("WARNING: Failed to ensure S3 lifecycle rule: {error}");
            false
        }
        Err(_) => {
            eprintln!("WARNING: Timed out ensuring S3 lifecycle rule");
            false
        }
    };
    if synchronized {
        *S3_LIFECYCLE_SYNCED_CONFIG_ID
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(config_id);
    }
}

async fn ensure_configured_s3_lifecycle(http: &Client, s3_cfg: &config::S3Config) {
    if !s3_lifecycle_needs_sync(Some(s3_cfg)) {
        return;
    }

    // Serialize policy updates and re-check after waiting. This prevents two
    // concurrent startup/Settings paths from issuing duplicate GET/PUT sequences.
    let _sync_guard = S3_LIFECYCLE_SYNC_LOCK.lock().await;
    if !s3_lifecycle_needs_sync(Some(s3_cfg)) {
        return;
    }
    synchronize_s3_lifecycle(http, s3_cfg).await;
}

async fn ensure_current_configured_s3_lifecycle(state: &AppState, requested_config_id: &str) {
    let _sync_guard = S3_LIFECYCLE_SYNC_LOCK.lock().await;
    let current_config = state.config();
    let Some(s3_cfg) = current_config.s3.as_ref() else {
        return;
    };
    // A newer Settings save may have applied while this request waited for the
    // lifecycle lock. Only the request matching the current S3 identity may
    // write the bucket policy; the newer save will synchronize after us when
    // it changed the identity while an older request was already in flight.
    if !s3_lifecycle_request_matches_current(Some(s3_cfg), requested_config_id)
        || !s3_lifecycle_needs_sync(Some(s3_cfg))
    {
        return;
    }
    synchronize_s3_lifecycle(&state.http, s3_cfg).await;
}

/// PUT /api/config — validate and save the JSON config file.
async fn api_put_config(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
    let base_config_file_etag = match body.get("baseConfigFileEtag") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(value))
            if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            Some(value.to_ascii_lowercase())
        }
        Some(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "baseConfigFileEtag must be a 64-character SHA-256 hex string"
                })),
            ));
        }
    };
    let config_value = body
        .get("config")
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "Missing 'config' field"})),
            )
        })?
        .clone();

    // Validate: must be a valid JSON object and deserializable as JsonConfig
    if !config_value.is_object() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "Config must be a JSON object"})),
        ));
    }
    let parsed = match serde_json::from_value::<config::JsonConfig>(config_value.clone()) {
        Ok(parsed) => parsed,
        Err(e) => {
            let msg = e.to_string();
            // Extract line/column info from serde error when available
            let (line, column) = parse_serde_error_position(&msg);
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": msg, "line": line, "column": column})),
            ));
        }
    };
    if let Err(error) = config::validate_json_provider_names(&parsed) {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": error}))));
    }
    if let Err(error) = config::validate_json_provider_models(&parsed) {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": error}))));
    }
    if let Err(error) = config::validate_json_agent_model_refs(&parsed) {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": error}))));
    }
    if let Err(error) = config::Config::validate_json_mcp_servers_for_workspace(
        &parsed,
        &session_workspace_path(MAIN_SESSION_ID),
    ) {
        return Err((StatusCode::BAD_REQUEST, Json(json!({"error": error}))));
    }

    let path = config_file_path().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Cannot determine config path"})),
        )
    })?;

    let pretty =
        serde_json::to_string_pretty(&config_value).unwrap_or_else(|_| config_value.to_string());

    let _save_guard = CONFIG_FILE_LOCK.write().await;
    if let Some(base_etag) = base_config_file_etag {
        let current_content = read_config_file_snapshot_unlocked(&path).map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": format!("Cannot read config: {error}")})),
            )
        })?;
        let current_etag = config_file_etag(&current_content);
        if base_etag != current_etag {
            let (_, current_revision) = state.config_snapshot_with_revision();
            let current_config = serde_json::from_str::<serde_json::Value>(&current_content).ok();
            return Err((
                StatusCode::CONFLICT,
                Json(json!({
                    "error": "Configuration changed after it was loaded. Reload the latest configuration before saving.",
                    "config": current_config,
                    "configRevision": current_revision,
                    "configFileEtag": current_etag,
                })),
            ));
        }
    }

    // Write to temp file then replace original without discarding the old file
    // if the final swap fails on Windows.
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, &pretty).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to write config: {e}")})),
        )
    })?;
    replace_file_from_temp(&path, &tmp_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Failed to finalize config: {e}")})),
        )
    })?;

    // Hot-reload: re-read the saved config into the runtime so that
    // model/MCP changes take effect without a restart.
    let new_config = Config::load();
    let (applied_config, config_revision) = state.apply_runtime_config(new_config);
    let s3_lifecycle_config_id = applied_config.s3.as_ref().and_then(|s3_cfg| {
        s3_lifecycle_needs_sync(Some(s3_cfg)).then(|| image_uploads::s3_config_id(s3_cfg))
    });

    // Collect an immutable same-revision snapshot for every connected Session
    // and Group while the write lock still protects this applied Config.
    let model_configuration_payloads =
        socket_sync::collect_model_configuration_payloads(&state, &applied_config, config_revision)
            .await;

    // Release the config file lock before any potentially backpressured socket
    // send or slow storage/MCP I/O.
    drop(_save_guard);

    // A first-run S3 configuration is hot-reloaded without restarting the
    // process. Ensure its lifecycle policy before the Settings save completes,
    // just as startup does, so newly enabled image tools do not depend on a
    // later restart to receive retention management.
    if let Some(config_id) = s3_lifecycle_config_id.as_deref() {
        ensure_current_configured_s3_lifecycle(&state, config_id).await;
    }

    socket_sync::send_model_configuration_payloads(&state, model_configuration_payloads).await;

    // Invalidate cached MCP runtime state so the next explicit catalog/agent
    // round sees config changes without probing session-disabled servers during
    // Settings Save.
    tools::mcp::invalidate_runtime_state_without_remote_shutdown().await;

    Ok(Json(json!({
        "ok": true,
        "environmentModelConfigured": environment_model_configured(),
        "explicitPrimaryModelConfigured": applied_config.explicit_primary_model_configured,
        "configuredModelsAvailable": configured_models_available(&applied_config),
        "configRevision": config_revision,
        "configFileEtag": config_file_etag(&pretty),
    })))
}

/// POST /api/config/test-model — test a model provider connection.
async fn api_test_model(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    const PLACEHOLDER_SAVE_REQUIRED: &str =
        "Save config before testing providers that use ${ENV} placeholders.";

    validate_local_request_headers(&headers)?;

    let base_url = body["baseUrl"].as_str().unwrap_or_default().to_string();
    let api_key = body["apiKey"].as_str().unwrap_or_default().to_string();
    let api = body["api"]
        .as_str()
        .unwrap_or("openai-completions")
        .to_string();
    let provider_name = body["providerName"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let model_id = body["modelId"].as_str().unwrap_or_default().to_string();

    if base_url.is_empty() || model_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "baseUrl and modelId are required"})),
        ));
    }

    let uses_placeholder =
        config::is_config_env_placeholder(&base_url) || config::is_config_env_placeholder(&api_key);

    let resolved = if uses_placeholder {
        let Some(provider_name) = provider_name else {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": PLACEHOLDER_SAVE_REQUIRED})),
            ));
        };

        let raw_cfg = config::load_config_file();
        if !config::provider_request_matches_saved_config(
            &raw_cfg,
            &provider_name,
            &api,
            &base_url,
            &api_key,
        ) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({"error": PLACEHOLDER_SAVE_REQUIRED})),
            ));
        }

        let runtime_config = state.config();
        if !runtime_config.providers.contains_key(&provider_name) {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!(
                        "Configured provider '{provider_name}' is unavailable at runtime. Check that referenced environment variables are set."
                    )
                })),
            ));
        }

        runtime_config.resolve_model(&format!("{provider_name}/{model_id}"))
    } else {
        let provider = Provider::from_api_kind(&api);
        providers::ResolvedModel {
            provider,
            api_base: base_url,
            api_key,
            model_id,
            reasoning: false,
            thinking_format: None,
            openai_responses_reasoning_summary: None,
            max_tokens: Some(16),
            context_window: 4096,
            stream_include_usage: false,
            anthropic_prompt_caching: false,
        }
    };

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some("Hi".to_string()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    match providers::call_llm_simple(&state.http, &resolved, &messages, &PathBuf::new(), None, 1)
        .await
    {
        Ok(reply) => Ok(Json(json!({"ok": true, "reply": truncate(&reply, 200)}))),
        Err(e) => {
            eprintln!("Model test failed: {e}");
            Ok(Json(json!({"ok": false, "error": truncate(&e, 200)})))
        }
    }
}

/// POST /api/config/test-mcp — test an MCP server connection.
async fn api_test_mcp(
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

    let server_name = body
        .get("server")
        .or_else(|| body.get("name"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("__test__");
    let command = body["command"].as_str().unwrap_or_default().to_string();
    let args: Vec<String> = body["args"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let env: HashMap<String, String> = body["env"]
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let timeout_secs = body["timeoutSecs"].as_u64();

    let cwd = body["cwd"].as_str().map(|s| s.to_string());

    let transport = body["transport"]
        .as_str()
        .map(str::trim)
        .unwrap_or_default();
    let url = body["url"].as_str().unwrap_or_default();
    let effective_transport = if transport.is_empty() {
        if !command.trim().is_empty() {
            "stdio".to_string()
        } else if !url.trim().is_empty() {
            "streamable-http".to_string()
        } else {
            "stdio".to_string()
        }
    } else {
        transport.to_ascii_lowercase()
    };
    if effective_transport != "stdio" && effective_transport != "streamable-http" {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "transport must be stdio or streamable-http"})),
        ));
    }
    if effective_transport == "stdio" && command.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "command is required"})),
        ));
    }
    if effective_transport == "streamable-http" && url.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "url is required"})),
        ));
    }

    let requested_session_id = query.session.as_deref().unwrap_or(MAIN_SESSION_ID);
    let session_id = ensure_session_loaded_for_api(&state, requested_session_id).await?;
    let workspace = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .map(|session| session.workspace.clone())
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({ "error": format!("Session '{}' not found", session_id) })),
                )
            })?
    };
    let auth = body
        .get("auth")
        .filter(|value| !value.is_null())
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|error| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("invalid auth config: {error}")})),
            )
        })?;
    let mcp_cfg = config::JsonMcpServerConfig {
        transport: body["transport"].as_str().map(|s| s.to_string()),
        command,
        url: body["url"].as_str().map(|s| s.to_string()),
        args,
        env,
        headers: body["headers"]
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
        cwd,
        enabled: true,
        auth,
        timeout_secs,
    };

    let config = state.config();
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(config.tool_timeout.as_secs()));
    match tokio::time::timeout(
        timeout,
        tools::mcp::test_mcp_server(server_name, &mcp_cfg, &workspace, config.tool_timeout),
    )
    .await
    {
        Ok(Ok(tool_count)) => Ok(Json(json!({"ok": true, "tools": tool_count}))),
        Ok(Err(e)) => Ok(Json(json!({"ok": false, "error": e}))),
        Err(_) => Ok(Json(json!({"ok": false, "error": "Connection timed out"}))),
    }
}

fn usage_snapshot_from_session(session: &mut Session) -> storage::SessionUsageSnapshot {
    context::rollover_daily_usage_if_needed(session);
    storage::SessionUsageSnapshot {
        daily_input: session.daily_input_tokens,
        daily_output: session.daily_output_tokens,
        total_input: session.input_tokens,
        total_output: session.output_tokens,
        input_source: session.input_token_source.clone(),
        output_source: session.output_token_source.clone(),
        usage_history: session.usage_history.clone(),
        daily_labels: session.daily_provider_usage.clone(),
        total_labels: session.total_label_usage.clone(),
    }
}

fn usage_api_payload(snapshot: Option<storage::SessionUsageSnapshot>) -> serde_json::Value {
    let snapshot = snapshot.unwrap_or_else(|| storage::SessionUsageSnapshot {
        input_source: default_token_usage_source(),
        output_source: default_token_usage_source(),
        ..Default::default()
    });
    let (daily_providers, daily_roles) = split_usage_labels(&snapshot.daily_labels);
    let (total_providers, total_roles) = split_usage_labels(&snapshot.total_labels);
    let usage_history = snapshot
        .usage_history
        .iter()
        .map(|day| {
            let (providers, roles) = split_usage_labels(&day.providers);
            json!({
                "date": day.date,
                "input": day.input,
                "output": day.output,
                "providers": providers,
                "roles": roles,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "daily_input": snapshot.daily_input,
        "daily_output": snapshot.daily_output,
        "total_input": snapshot.total_input,
        "total_output": snapshot.total_output,
        "total": snapshot.total_input.saturating_add(snapshot.total_output),
        "input_source": snapshot.input_source,
        "output_source": snapshot.output_source,
        "source_scope": "latest_update",
        "usage_history": usage_history,
        "daily_providers": daily_providers,
        "daily_roles": daily_roles,
        "total_providers": total_providers,
        "total_roles": total_roles,
    })
}

/// GET /api/usage — token usage statistics.
async fn api_usage(
    Query(query): Query<SessionQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

    let session_id = match query.session.as_deref() {
        Some(requested) => crate::session_store::validate_session_id(requested)
            .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?
            .to_string(),
        None => MAIN_SESSION_ID.to_string(),
    };

    #[cfg(test)]
    let snapshot = {
        let mut sessions = state.sessions.lock().await;
        if !sessions.contains_key(session_id.as_str())
            && let Some(session) = load_session_from_disk(&session_id)
        {
            sessions.insert(session_id.clone(), session);
        }
        sessions
            .get_mut(session_id.as_str())
            .map(usage_snapshot_from_session)
    };

    #[cfg(not(test))]
    let snapshot = {
        let loaded = {
            let mut sessions = state.sessions.lock().await;
            let loaded_id = sessions
                .keys()
                .find(|loaded_id| crate::session_ids_match(loaded_id, &session_id))
                .cloned();
            loaded_id
                .and_then(|loaded_id| sessions.get_mut(&loaded_id))
                .map(usage_snapshot_from_session)
        };
        match loaded {
            Some(snapshot) => Some(snapshot),
            None => state
                .storage
                .load_usage_snapshot(
                    &session_id,
                    &crate::prompts::current_local_snapshot().today(),
                )
                .await
                .map_err(|error| {
                    eprintln!(
                        "ERROR: failed to load usage for Session '{}': {error}",
                        session_id
                    );
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({
                            "error": "Failed to load usage data.",
                            "code": "storage_read_failed",
                        })),
                    )
                })?,
        }
    };

    Ok(Json(usage_api_payload(snapshot)))
}

fn read_config_file_snapshot_unlocked(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok("{}".to_string()),
        Err(err) => Err(err),
    }
}

fn config_file_etag(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

#[cfg(test)]
async fn read_config_file_snapshot(path: &Path) -> std::io::Result<String> {
    let _read_guard = CONFIG_FILE_LOCK.read().await;
    read_config_file_snapshot_unlocked(path)
}

async fn read_runtime_config_file_snapshot(
    state: &AppState,
    path: &Path,
) -> std::io::Result<(Arc<Config>, u64, String)> {
    let _read_guard = CONFIG_FILE_LOCK.read().await;
    let (config, config_revision) = state.config_snapshot_with_revision();
    let content = read_config_file_snapshot_unlocked(path)?;
    Ok((config, config_revision, content))
}

fn parse_serde_error_position(msg: &str) -> (Option<u64>, Option<u64>) {
    // serde_json errors: "... at line X column Y"
    static RE: std::sync::LazyLock<Option<regex::Regex>> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"line (\d+) column (\d+)").ok());
    if let Some(re) = RE.as_ref()
        && let Some(caps) = re.captures(msg)
    {
        let line = caps.get(1).and_then(|m| m.as_str().parse().ok());
        let col = caps.get(2).and_then(|m| m.as_str().parse().ok());
        return (line, col);
    }
    (None, None)
}

fn replace_file_from_temp(path: &Path, tmp_path: &Path) -> std::io::Result<()> {
    match std::fs::rename(tmp_path, path) {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            if !path.exists() {
                return Err(rename_err);
            }

            let mut backup_name = path
                .file_name()
                .map(|name| name.to_os_string())
                .unwrap_or_else(|| std::ffi::OsString::from("config"));
            backup_name.push(".lingclaw-save-backup");
            let backup_path = path.with_file_name(backup_name);

            match std::fs::remove_file(&backup_path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(err),
            }

            std::fs::rename(path, &backup_path)?;

            match std::fs::rename(tmp_path, path) {
                Ok(()) => {
                    if let Err(err) = std::fs::remove_file(&backup_path)
                        && err.kind() != std::io::ErrorKind::NotFound
                    {
                        eprintln!(
                            "Warning: failed to remove temporary config backup {}: {err}",
                            backup_path.display()
                        );
                    }
                    Ok(())
                }
                Err(finalize_err) => {
                    if let Err(restore_err) = std::fs::rename(&backup_path, path) {
                        return Err(std::io::Error::new(
                            finalize_err.kind(),
                            format!(
                                "{finalize_err}; failed to restore previous config: {restore_err}"
                            ),
                        ));
                    }
                    Err(finalize_err)
                }
            }
        }
    }
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

    if args.get(1).map(String::as_str) == Some("db") {
        storage::handle_db_cli(&args[2..]);
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
    runtime_loop::refresh_reflection_runtime(config.daily_reflection);
    let port = port_override.unwrap_or(config.port);

    if config.api_key.is_empty()
        && config.providers.is_empty()
        && config.provider.api_key_env_hint().is_some()
    {
        eprintln!(
            "WARNING: {} is not set and no config file providers found. LLM calls will fail.",
            config.provider.api_key_env_hint().unwrap_or("API key")
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
    let mcp_enabled = config
        .mcp_servers
        .values()
        .filter(|server| server.enabled)
        .count();
    if mcp_enabled > 0 {
        eprintln!("  MCP servers:   {} enabled", mcp_enabled);
    }
    eprintln!("  Exec timeout:  {}s", config.exec_timeout.as_secs());
    eprintln!("  Tool timeout:  {}s", config.tool_timeout.as_secs());
    eprintln!(
        "  Agent timeout: {}",
        crate::config::format_sub_agent_timeout(config.sub_agent_timeout)
    );
    eprintln!("  LLM retries:  {}", config.max_llm_retries);
    eprintln!(
        "  Context limit: {} tokens",
        config.context_limit_for_model(&config.model)
    );

    let database_path = match storage::Database::default_path() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("ERROR: Failed to resolve SQLite storage path: {error}");
            std::process::exit(1);
        }
    };
    let _runtime_instance_lock = match storage::RuntimeInstanceLock::acquire(&database_path) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("ERROR: Failed to acquire LingClaw runtime ownership: {error}");
            std::process::exit(1);
        }
    };
    let addr = format!("127.0.0.1:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("Failed to bind {addr}: {error}");
            return;
        }
    };
    if let Err(error) = storage::preflight_legacy_storage_path_conflicts(&database_path) {
        eprintln!("ERROR: Legacy storage preflight failed: {error}");
        std::process::exit(1);
    }
    let database = match storage::Database::open(database_path).await {
        Ok(database) => database,
        Err(error) => {
            eprintln!("ERROR: Failed to initialize SQLite storage: {error}");
            std::process::exit(1);
        }
    };
    match storage::migrate_legacy_json_if_needed(&database).await {
        Ok(Some(backup)) => {
            eprintln!(
                "  Storage:       migrated legacy JSON to SQLite (backup: {})",
                backup.display()
            );
        }
        Ok(None) => eprintln!("  Storage:       {}", database.path().display()),
        Err(error) => {
            eprintln!("ERROR: Legacy storage migration failed: {error}");
            std::process::exit(1);
        }
    }
    match database.recover_interrupted_plans().await {
        Ok((planning, executing)) if planning + executing > 0 => {
            eprintln!(
                "  Storage:       recovered {} interrupted plan(s) ({} planning, {} executing)",
                planning + executing,
                planning,
                executing
            );
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("ERROR: Failed to recover interrupted plans: {error}");
            std::process::exit(1);
        }
    }
    if let Err(error) = database.install_global() {
        eprintln!("ERROR: Failed to activate SQLite storage: {error}");
        std::process::exit(1);
    }
    #[cfg(not(test))]
    let storage_status_rx = database.subscribe_status();

    let shutdown = CancellationToken::new();

    // Generate a one-time shutdown token and write it to disk for CLI use
    let shutdown_token = match generate_shutdown_token() {
        Ok(token) => token,
        Err(error) => {
            eprintln!("ERROR: {error}");
            return;
        }
    };
    let upload_token = match generate_secret_token() {
        Ok(token) => token,
        Err(error) => {
            eprintln!("ERROR: {error}");
            return;
        }
    };
    if let Some(dir) = config_dir_path() {
        let _ = std::fs::write(dir.join(format!("shutdown-{port}.token")), &shutdown_token);
    }

    let mut hooks = HookRegistry::new();
    hooks.register(Box::new(AutoCompressContextHook::new()));

    let sessions = Arc::new(Mutex::new(HashMap::new()));

    let memory_queue = if config.structured_memory {
        Some(MemoryUpdateQueue::spawn(config.clone(), sessions.clone()))
    } else {
        None
    };

    let http = Client::new();
    if let Some(s3_cfg) = config.s3.as_ref() {
        ensure_configured_s3_lifecycle(&http, s3_cfg).await;
    }

    let state = Arc::new(AppState {
        #[cfg(not(test))]
        storage: database,
        config: std::sync::Mutex::new(Arc::new(config)),
        http,
        sessions,
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        group_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        session_control_locks: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: shutdown.clone(),
        shutdown_token,
        upload_token,
        hooks,
        memory_queue: std::sync::Mutex::new(memory_queue),
    });
    #[cfg(not(test))]
    tokio::spawn(monitor_storage_status(state.clone(), storage_status_rx));

    match ensure_session_ready(&state, None).await {
        Ok((session_id, _)) => eprintln!("  Default session: {session_id} ready"),
        Err(error) => {
            eprintln!("Failed to initialize default session: {error}");
            if !should_continue_after_default_session_error(&state.storage_status()) {
                return;
            }
            eprintln!("  Storage:       protected; continuing with read-only recovery access");
        }
    }

    let static_dir = resolve_static_dir();
    eprintln!("  Static dir:    {}", static_dir.display());

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/health", get(api_health))
        .route("/api/client-config", get(api_client_config))
        .route("/api/sessions", get(api_sessions))
        .route("/api/session", post(api_post_session).put(api_put_session))
        .route(
            "/api/session-groups",
            get(session_control::api_session_groups),
        )
        .route(
            "/api/session-group",
            get(session_control::api_get_session_group)
                .post(session_control::api_post_session_group)
                .put(session_control::api_put_session_group)
                .delete(session_control::api_delete_session_group),
        )
        .route(
            "/api/session-group/member",
            put(session_control::api_promote_session_group_admin)
                .delete(session_control::api_delete_session_group_member),
        )
        .route(
            "/api/session-skills",
            get(api_session_skills).put(api_put_session_skills),
        )
        .route("/api/mcp/catalog", get(api_mcp_catalog))
        .route("/api/mcp/session-policy", put(api_put_mcp_session_policy))
        .route("/api/mcp/auth/start", post(api_mcp_auth_start))
        .route(
            "/api/mcp/auth/callback",
            get(api_mcp_auth_callback_get).post(api_mcp_auth_callback_post),
        )
        .route("/api/mcp/auth/disconnect", post(api_mcp_auth_disconnect))
        .route("/api/mcp/resource/read", post(api_mcp_resource_read))
        .route("/api/mcp/prompt/get", post(api_mcp_prompt_get))
        .route("/api/todos", put(api_todos))
        .route("/api/config", get(api_get_config).put(api_put_config))
        .route("/api/config/test-model", post(api_test_model))
        .route("/api/config/test-mcp", post(api_test_mcp))
        .route("/api/usage", get(api_usage))
        .route(
            "/api/upload-images",
            post(api_upload_images).layer(DefaultBodyLimit::max(
                image_uploads::MAX_IMAGE_UPLOAD_REQUEST_BYTES,
            )),
        )
        .route("/api/shutdown", post(api_shutdown))
        .fallback_service(ServeDir::new(static_dir).append_index_html_on_directories(true))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            enforce_storage_writable,
        ))
        .layer(middleware::from_fn(enforce_local_request))
        .with_state(state.clone());

    println!("🦀 LingClaw v2 listening on http://{addr}");
    println!(
        "   Tools: think, todos, exec, read_file, write_file, patch_file, list_dir, search_files, http_fetch"
    );

    match session_group::recover_stale_group_runs_on_startup().await {
        Ok(recovered) if recovered > 0 => {
            eprintln!("Recovered {recovered} stale session group run(s) from a previous process.");
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("WARNING: Failed to recover stale session group runs: {error}");
        }
    }

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
    let session_ids: Vec<String> = {
        let guard = state.sessions.lock().await;
        guard
            .iter()
            .filter(|&(_, session)| session.messages.len() > 1)
            .map(|(session_id, _)| session_id.clone())
            .collect()
    };
    for session_id in &session_ids {
        let _ = session_store::save_current_session_to_disk(&state, session_id).await;
    }
    #[cfg(not(test))]
    if let Err(error) = state.storage.checkpoint().await {
        eprintln!("WARNING: Failed to checkpoint SQLite storage: {error}");
    }
    // Clean up shutdown token file
    if let Some(dir) = config_dir_path() {
        let _ = std::fs::remove_file(dir.join(format!("shutdown-{port}.token")));
    }
    eprintln!("Server shut down, {} session(s) saved.", session_ids.len());
}

#[cfg(test)]
#[path = "tests/main_tests.rs"]
mod main_tests;
