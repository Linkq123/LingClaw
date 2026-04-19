use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Multipart, Request, State,
        ws::{Message as WsMsg, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, mpsc};
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
mod prompts;
mod providers;
mod runtime_loop;
mod session_admin;
mod session_store;
mod socket_sync;
mod socket_tasks;
mod subagents;
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
    IdleSocketInputAction, handle_idle_socket_input, resolve_or_create_socket_session,
    run_agent_session,
};
use session_store::{load_session_from_disk, refresh_session_system_prompt, save_session_to_disk};
use socket_sync::{
    build_session_info_payload, send_command_refresh, send_existing_session_payloads,
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
use session_store::{
    build_active_session_lines, build_global_today_usage, build_history_payload,
    build_session_status, build_session_usage, build_usage_report, list_saved_session_ids_in_dir,
    list_saved_session_summaries_in_dir, recoverable_session_ids_from_summaries,
    resolve_session_target, sanitize_session_messages, sessions_dir, trim_incomplete_tool_calls,
};
use std::collections::HashSet;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const MAIN_SESSION_ID: &str = "main";
const INBOUND_BUFFER_CAPACITY: usize = 128;
static CONFIG_FILE_LOCK: std::sync::LazyLock<tokio::sync::RwLock<()>> =
    std::sync::LazyLock::new(|| tokio::sync::RwLock::new(()));

// ── Data Models ──────────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug)]
struct ImageAttachment {
    url: String,
    /// Persisted S3 object key for locally uploaded images so fresh
    /// presigned URLs can be generated for history replay and provider calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    s3_object_key: Option<String>,
    /// Persisted path to a cached base64 file inside the session workspace.
    /// This keeps historical Ollama images available across restarts without
    /// bloating the session JSON itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cache_path: Option<String>,
    /// Cached base64-encoded image data.  Populated at intake so Ollama
    /// requests never re-fetch historical URLs.  Not persisted to disk
    /// (`skip_serializing`) to avoid bloating session files; after a reload
    /// the disk cache or legacy network fallback in `fetch_images_base64`
    /// handles it.
    #[serde(skip_serializing, default)]
    data: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    images: Option<Vec<ImageAttachment>>,
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

const SESSION_VERSION: u32 = 4;

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
    /// System skill paths disabled for this session (e.g. "anthropics", "anthropics/pdf").
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    disabled_system_skills: HashSet<String>,
    /// Tool call ids whose persisted tool result ended in an error state.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    failed_tool_results: HashSet<String>,
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
            disabled_system_skills: HashSet::new(),
            failed_tool_results: HashSet::new(),
            version: SESSION_VERSION,
            workspace,
        }
    }

    fn effective_model<'a>(&'a self, default: &'a str) -> &'a str {
        self.model_override.as_deref().unwrap_or(default)
    }
}

const UPLOAD_TOKEN_HEADER: &str = "x-lingclaw-upload-token";

struct AppState {
    config: std::sync::Mutex<Arc<Config>>,
    http: Client,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    /// Session IDs with the connection currently attached to live streaming output.
    active_connections: Mutex<HashMap<String, u64>>,
    session_clients: Mutex<HashMap<String, SessionClientBinding>>,
    live_rounds: Mutex<HashMap<String, LiveRoundState>>,
    /// Per-session active agent runs keyed by the owning connection.
    active_runs: Mutex<HashMap<String, SessionRunBinding>>,
    /// Per-session connection-level cancellation tokens (kick old connection on rebind).
    connection_cancels: Mutex<HashMap<String, ConnectionCancelBinding>>,
    next_connection_id: AtomicU64,
    shutdown: CancellationToken,
    shutdown_token: String,
    upload_token: String,
    hooks: HookRegistry,
    /// Background structured memory updater (active when config.structured_memory is true).
    memory_queue: Option<MemoryUpdateQueue>,
}

impl AppState {
    /// Return a snapshot of the current runtime config.
    fn config(&self) -> Arc<Config> {
        self.config.lock().expect("config lock poisoned").clone()
    }

    /// Hot-swap the runtime config (called after saving to disk).
    fn replace_config(&self, new: Config) {
        *self.config.lock().expect("config lock poisoned") = Arc::new(new);
    }
}

#[derive(Clone)]
struct SessionClientBinding {
    connection_id: u64,
    tx: WsTx,
    replay_ready: bool,
    pending_events: Vec<serde_json::Value>,
}

#[derive(Clone)]
struct SessionRunBinding {
    connection_id: u64,
    cancel: CancellationToken,
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
    result: Option<String>,
    elapsed_ms: u64,
}

#[derive(Clone, Default)]
struct LiveRoundState {
    connection_id: u64,
    round: usize,
    react_visible: bool,
    phase: Option<String>,
    cycle: Option<usize>,
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

/// Cap for replay buffer strings (128 KB). Keeps memory bounded for long outputs.
const LIVE_REPLAY_CAP: usize = 128 * 1024;
/// Max delegated events kept per round. Prevents unbounded memory growth for
/// long-running rounds with many sub-agent / orchestration events.
const DELEGATED_EVENTS_CAP: usize = 10_000;
const TOOL_PROGRESS_HEARTBEAT_SECS: u64 = 1;

// ── System Prompt ────────────────────────────────────────────────────────────

fn build_system_prompt(
    config: &Config,
    workspace: &Path,
    model: &str,
    disabled_system_skills: &HashSet<String>,
) -> ChatMessage {
    build_system_prompt_with_query(config, workspace, model, disabled_system_skills, None)
}

fn build_system_prompt_with_query(
    config: &Config,
    workspace: &Path,
    model: &str,
    disabled_system_skills: &HashSet<String>,
    current_query: Option<&str>,
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
Do not call file tools just to verify or re-read BOOTSTRAP.md, AGENTS.md, AGENT.md, IDENTITY.md, USER.md, SOUL.md, TOOLS.md, or MEMORY.md when their content is already present below.\n\
Only read those files if the user explicitly asks to inspect them, if you need to edit them, or if a task depends on checking whether the on-disk file has changed.";
    let mcp_note = tools::mcp::runtime_tool_note(config)
        .map(|note| format!("\n\n## MCP Runtime\n- {note}"))
        .unwrap_or_default();

    let skills_section = prompts::discover_all_skills(workspace);
    let skills_section: Vec<_> = if disabled_system_skills.is_empty() {
        skills_section
    } else {
        skills_section
            .into_iter()
            .filter(|s| {
                s.source != prompts::SkillSource::System
                    || !prompts::is_system_skill_disabled(&s.path, disabled_system_skills)
            })
            .collect()
    };
    let skills_section = prompts::render_skills_catalog(&skills_section, current_query)
        .map(|s| format!("\n\n{s}"))
        .unwrap_or_default();

    // Structured memory injection (coexists with MEMORY.md and daily memory)
    let structured_memory_section = if config.structured_memory {
        memory::format_memory_for_injection(
            &memory::load_structured_memory(workspace),
            current_query,
        )
        .map(|s| format!("\n\n{s}"))
        .unwrap_or_default()
    } else {
        String::new()
    };

    // Sub-agent catalog (discovered from system/global/session layers)
    let agents_section = {
        let agents = subagents::discovery::discover_all_agents(workspace);
        subagents::render_agents_catalog(&agents)
            .map(|s| format!("\n\n{s}"))
            .unwrap_or_default()
    };

    let prompt = format!(
        r#"{persona}{structured_memory_section}

---

## Environment
- OS: {os_name}
- Current system local time: {local_time}
- Working directory: {cwd}
- Model: {model}

{prompt_file_note}

## Agent Behavior

You operate in a ReAct loop: **Analyze** the situation, **Act** by calling tools, **Observe** the results, then either loop or **Finish**.

- **Tool strategy:** Prefer calling tools to gather information over speculating. Batch independent read-only calls together. Run write operations one at a time.
- **Error recovery:** When a tool fails, diagnose the cause and try a different approach — different arguments, a different tool, or an alternative path. Do not repeat the same failing call.
- **Delegation:** For complex, self-contained subtasks, delegate to a sub-agent via the `task` tool. Handle simple, quick work yourself.
- **Finishing:** When the task is complete, deliver your result. When you are genuinely stuck with no further options, say so honestly. Do not pad with speculative follow-ups.

## Available Tools
{tool_lines}{mcp_note}{skills_section}{agents_section}"#,
        model = model,
        local_time = local_time,
        tool_lines = tool_lines,
        persona = persona,
        prompt_file_note = prompt_file_note,
        mcp_note = mcp_note,
        skills_section = skills_section,
        structured_memory_section = structured_memory_section,
        agents_section = agents_section,
    );

    ChatMessage {
        role: "system".into(),
        content: Some(prompt),
        images: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }
}

// ── Security ─────────────────────────────────────────────────────────────────────────────

const DANGEROUS_PATTERNS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "rm -rf ~",
    "mkfs.",
    "dd if=/dev",
    ":(){ :|:&",
    "> /dev/sda",
    "chmod -r 777 /",
    "chown -r root",
    "format c:",
    "del /f /s /q c:\\",
    "rd /s /q c:\\",
    "reg delete hk",
];

/// Collapse repeated whitespace to a single space for robust pattern matching.
fn normalize_command_whitespace(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn check_dangerous_command(cmd: &str) -> Option<&'static str> {
    let lower = normalize_command_whitespace(cmd).to_lowercase();
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
                if let Ok(meta) = std::fs::symlink_metadata(&resolved)
                    && meta.file_type().is_symlink()
                {
                    eprintln!(
                        "SECURITY: path '{}' traverses symlink '{}', clamped",
                        path_str,
                        resolved.display()
                    );
                    return ws_canonical;
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

    if relative.components().any(|component| {
        matches!(component, std::path::Component::Normal(part) if part == ".lingclaw-bootstrap")
    }) {
        return Err(format!(
            "path '{}' targets protected internal workspace data",
            path_str
        ));
    }

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
                if let Ok(meta) = std::fs::symlink_metadata(&resolved)
                    && meta.file_type().is_symlink()
                {
                    return Err(format!(
                        "path '{}' traverses symlink '{}' outside the session workspace '{}'",
                        path_str,
                        resolved.display(),
                        workspace_root.display()
                    ));
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

fn generate_secret_token() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| format!("failed to get secure random bytes for secret token: {e}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn generate_shutdown_token() -> Result<String, String> {
    generate_secret_token()
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

fn validate_local_request_headers(
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

async fn enforce_local_request(
    request: Request,
    next: Next,
) -> Result<Response, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(request.headers())?;
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

async fn dispatch_live_event(
    state: &AppState,
    session_id: &str,
    connection_id: u64,
    event: serde_json::Value,
) {
    let event_type = event["type"].as_str().unwrap_or_default();
    let mut delegated_replay_event: Option<serde_json::Value> = None;

    // Validate connection ownership and update live replay state under a single
    // critical section. We hold session_clients for the entire block to prevent
    // unbind/rebind from racing between validation and live_rounds mutation.
    {
        let clients_guard = state.session_clients.lock().await;
        let is_current = clients_guard
            .get(session_id)
            .map(|b| b.connection_id == connection_id)
            .unwrap_or(false);
        if !is_current {
            return;
        }

        let mut live_rounds = state.live_rounds.lock().await;
        // Drop the clients guard now — we've entered the live_rounds critical
        // section and no longer need the binding check to stay valid.
        drop(clients_guard);

        match event_type {
            "start" => {
                live_rounds.insert(
                    session_id.to_string(),
                    LiveRoundState {
                        connection_id,
                        round: event["round"].as_u64().unwrap_or(1) as usize,
                        react_visible: event["react_visible"].as_bool().unwrap_or(false),
                        phase: event["phase"].as_str().map(str::to_string),
                        cycle: event["cycle"].as_u64().map(|value| value as usize),
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
            "thinking_start" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                {
                    round.reasoning_text.clear();
                    round.reasoning_done = false;
                }
            }
            "thinking_delta" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                    && let Some(content) = event["content"].as_str()
                    && round.reasoning_text.len() < LIVE_REPLAY_CAP
                {
                    round.reasoning_text.push_str(content);
                    truncate_safe(&mut round.reasoning_text, LIVE_REPLAY_CAP);
                }
            }
            "thinking_done" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                {
                    round.reasoning_done = true;
                }
            }
            "tool_call" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && !is_subagent_live_event(&event)
                {
                    round.tools.push(LiveToolState {
                        id: event["id"].as_str().unwrap_or_default().to_string(),
                        name: event["name"].as_str().unwrap_or_default().to_string(),
                        arguments: event["arguments"].as_str().unwrap_or_default().to_string(),
                        result: None,
                        elapsed_ms: 0,
                    });
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
                            result: None,
                            elapsed_ms,
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
                        if let Some(tool) = round.tools.iter_mut().find(|tool| tool.id == tool_id) {
                            tool.result = Some(result);
                            tool.elapsed_ms =
                                event["duration_ms"].as_u64().unwrap_or(tool.elapsed_ms);
                        } else {
                            round.tools.push(LiveToolState {
                                id: tool_id.to_string(),
                                name: event["name"].as_str().unwrap_or_default().to_string(),
                                arguments: String::new(),
                                result: Some(result),
                                elapsed_ms: event["duration_ms"].as_u64().unwrap_or(0),
                            });
                        }
                    }
                }
            }
            "react_phase" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                {
                    round.phase = event["phase"].as_str().map(str::to_string);
                    round.cycle = event["cycle"].as_u64().map(|value| value as usize);
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
                    && live_task_key_from_event(&event).is_some()
                {
                    delegated_replay_event = Some(event.clone());
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
    }

    let binding = {
        let mut clients = state.session_clients.lock().await;
        if let Some(binding) = clients.get_mut(session_id) {
            if binding.connection_id != connection_id {
                return;
            }
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
    if let Some(binding) = binding
        && !ws_send(&binding.tx, &event).await
    {
        unbind_session_connection_if_matches(state, session_id, binding.connection_id).await;
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
        if tool.result.is_none() && tool.elapsed_ms > 0 {
            ws_send(
                tx,
                &json!({
                    "type":"tool_progress",
                    "id": tool.id,
                    "name": tool.name,
                    "elapsed_ms": tool.elapsed_ms,
                }),
            )
            .await;
        }
        if let Some(result) = &tool.result {
            ws_send(
                tx,
                &json!({
                    "type":"tool_result",
                    "id": tool.id,
                    "name": tool.name,
                    "result": result,
                    "duration_ms": tool.elapsed_ms,
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

    for event in &live_round.delegated_events {
        ws_send(tx, event).await;
    }
}

// ── WebSocket Handler ────────────────────────────────────────────────────────

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state, None))
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

    let mut current_session_id =
        resolve_or_create_socket_session(&state, &tx, requested_id.as_deref(), connection_id).await;

    // Kick out any previous connection bound to this session.
    {
        let mut cancels = state.connection_cancels.lock().await;
        if let Some(old_binding) = cancels.remove(&current_session_id) {
            old_binding.cancel.cancel();
        }
        cancels.insert(
            current_session_id.clone(),
            ConnectionCancelBinding {
                connection_id,
                cancel: connection_cancel.clone(),
            },
        );
    }
    {
        let active_run = {
            let runs = state.active_runs.lock().await;
            runs.get(&current_session_id).cloned()
        };
        if let Some(run) = active_run
            && run.connection_id != connection_id
        {
            run.cancel.cancel();
        }
    }

    bind_session_connection(&state, &current_session_id, connection_id, &tx, false).await;
    replay_live_round(&tx, &state, &current_session_id).await;
    finish_session_replay(&state, &current_session_id, connection_id).await;

    let cancel = state.shutdown.clone();
    let current_session_ref = Arc::new(Mutex::new(current_session_id.clone()));
    let (live_tx, socket_tasks) = spawn_connection_tasks(
        state.clone(),
        connection_cancel.clone(),
        current_session_ref.clone(),
        connection_id,
    );

    let mut rerun_agent = false;
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
                &cancel,
            )
            .await
            {
                IdleSocketInputAction::Continue => continue,
                IdleSocketInputAction::StartAgent => {}
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
    let config = state.config();
    Json(json!({
        "status": "ok",
        "version": VERSION,
        "model": config.model,
        "sessions": sessions.len(),
    }))
}

async fn api_sessions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sessions = state.sessions.lock().await;
    let config = state.config();
    let list: Vec<serde_json::Value> = sessions
        .get(MAIN_SESSION_ID)
        .map(|s| {
            json!({
                "id": MAIN_SESSION_ID,
                "name": s.name,
                "messages": s.messages.len(),
                "tool_calls": s.tool_calls_count,
                "model": s.effective_model(&config.model),
                "created_at": s.created_at,
                "updated_at": s.updated_at,
            })
        })
        .into_iter()
        .collect();
    Json(json!({"sessions": list}))
}

async fn api_client_config(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
    Ok(Json(json!({
        "upload_token": state.upload_token,
    })))
}

/// POST /api/upload-images — multipart image upload to S3.
async fn api_upload_images(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
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

    let mut uploaded_images: Vec<serde_json::Value> = Vec::new();
    let mut urls: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let max_files = image_uploads::MAX_IMAGE_UPLOAD_FILES;

    loop {
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

        match image_uploads::s3_presigned_get_url(s3_cfg, &object_key) {
            Ok(url) => {
                let attachment_token =
                    image_uploads::sign_attachment_object_key(s3_cfg, &object_key);
                uploaded_images.push(json!({
                    "url": url.clone(),
                    "object_key": object_key,
                    "attachment_token": attachment_token,
                }));
                urls.push(url);
            }
            Err(e) => errors.push(e),
        }
    }

    Ok(Json(
        json!({ "images": uploaded_images, "urls": urls, "errors": errors }),
    ))
}

// ── Config & Usage API ───────────────────────────────────────────────────────

/// GET /api/config — read the raw JSON config file.
async fn api_get_config(
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
    let path = config_file_path().ok_or_else(|| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "Cannot determine config path"})),
        )
    })?;
    let content = read_config_file_snapshot(&path).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("Cannot read config: {e}")})),
        )
    })?;
    match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(value) => Ok(Json(json!({
            "config": value,
            "path": path.display().to_string(),
        }))),
        Err(e) => {
            let msg = e.to_string();
            let (line, column) = parse_serde_error_position(&msg);
            Ok(Json(json!({
                "config": null,
                "raw": content,
                "path": path.display().to_string(),
                "parse_error": msg,
                "line": line,
                "column": column,
            })))
        }
    }
}

/// PUT /api/config — validate and save the JSON config file.
async fn api_put_config(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;
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
    state.replace_config(new_config);

    // Release the config file lock before potentially slow MCP I/O.
    drop(_save_guard);

    // Invalidate cached MCP tool definitions so the next round picks up
    // any server additions/removals.
    let workspace = session_workspace_path(MAIN_SESSION_ID);
    let _ = tools::mcp::refresh_servers(&state.config(), &workspace).await;

    Ok(Json(json!({"ok": true})))
}

/// POST /api/config/test-model — test a model provider connection.
async fn api_test_model(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

    let base_url = body["baseUrl"].as_str().unwrap_or_default().to_string();
    let api_key = body["apiKey"].as_str().unwrap_or_default().to_string();
    let api = body["api"]
        .as_str()
        .unwrap_or("openai-completions")
        .to_string();
    let model_id = body["modelId"].as_str().unwrap_or_default().to_string();

    if base_url.is_empty() || model_id.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "baseUrl and modelId are required"})),
        ));
    }

    let provider = Provider::from_api_kind(&api);
    let resolved = providers::ResolvedModel {
        provider,
        api_base: base_url,
        api_key,
        model_id,
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(16),
        context_window: 4096,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };

    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: Some("Hi".to_string()),
        images: None,
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
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

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

    if command.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "command is required"})),
        ));
    }

    let workspace = session_workspace_path(MAIN_SESSION_ID);
    let mcp_cfg = config::JsonMcpServerConfig {
        command,
        args,
        env,
        cwd,
        enabled: true,
        timeout_secs,
    };

    let config = state.config();
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(config.tool_timeout.as_secs()));
    match tokio::time::timeout(
        timeout,
        tools::mcp::test_mcp_server(&mcp_cfg, &workspace, config.tool_timeout),
    )
    .await
    {
        Ok(Ok(tool_count)) => Ok(Json(json!({"ok": true, "tools": tool_count}))),
        Ok(Err(e)) => Ok(Json(json!({"ok": false, "error": e}))),
        Err(_) => Ok(Json(json!({"ok": false, "error": "Connection timed out"}))),
    }
}

/// GET /api/usage — token usage statistics.
async fn api_usage(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    validate_local_request_headers(&headers)?;

    let mut sessions = state.sessions.lock().await;
    let session = sessions.get_mut(MAIN_SESSION_ID);
    let (
        daily_input,
        daily_output,
        total_input,
        total_output,
        input_source,
        output_source,
        usage_history,
        daily_providers,
        daily_roles,
        total_providers,
        total_roles,
    ) = if let Some(session) = session {
        context::rollover_daily_usage_if_needed(session);
        let (daily_providers, daily_roles) = split_usage_labels(&session.daily_provider_usage);
        let (total_providers, total_roles) = split_usage_labels(&session.total_label_usage);
        let usage_history = session
            .usage_history
            .iter()
            .map(|snapshot| {
                let (providers, roles) = split_usage_labels(&snapshot.providers);
                json!({
                    "date": snapshot.date,
                    "input": snapshot.input,
                    "output": snapshot.output,
                    "providers": providers,
                    "roles": roles,
                })
            })
            .collect::<Vec<_>>();
        (
            session.daily_input_tokens,
            session.daily_output_tokens,
            session.input_tokens,
            session.output_tokens,
            session.input_token_source.clone(),
            session.output_token_source.clone(),
            serde_json::to_value(usage_history).unwrap_or_else(|_| json!([])),
            serde_json::to_value(daily_providers).unwrap_or_else(|_| json!({})),
            serde_json::to_value(daily_roles).unwrap_or_else(|_| json!({})),
            serde_json::to_value(total_providers).unwrap_or_else(|_| json!({})),
            serde_json::to_value(total_roles).unwrap_or_else(|_| json!({})),
        )
    } else {
        (
            0,
            0,
            0,
            0,
            default_token_usage_source(),
            default_token_usage_source(),
            json!([]),
            json!({}),
            json!({}),
            json!({}),
            json!({}),
        )
    };

    Ok(Json(json!({
        "daily_input": daily_input,
        "daily_output": daily_output,
        "total_input": total_input,
        "total_output": total_output,
        "total": total_input.saturating_add(total_output),
        "input_source": input_source,
        "output_source": output_source,
        "source_scope": "latest_update",
        "usage_history": usage_history,
        "daily_providers": daily_providers,
        "daily_roles": daily_roles,
        "total_providers": total_providers,
        "total_roles": total_roles,
    })))
}

async fn read_config_file_snapshot(path: &Path) -> std::io::Result<String> {
    let _read_guard = CONFIG_FILE_LOCK.read().await;
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok("{}".to_string()),
        Err(err) => Err(err),
    }
}

fn parse_serde_error_position(msg: &str) -> (Option<u64>, Option<u64>) {
    // serde_json errors: "... at line X column Y"
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"line (\d+) column (\d+)").unwrap());
    if let Some(caps) = RE.captures(msg) {
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

    if config.api_key.is_empty()
        && config.providers.is_empty()
        && config.provider.api_key_env_var().is_some()
    {
        eprintln!(
            "WARNING: {} is not set and no config file providers found. LLM calls will fail.",
            config.provider.api_key_env_var().unwrap_or("API key")
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
    if let Some(s3_cfg) = config.s3.clone()
        && s3_cfg.lifecycle_days > 0
    {
        match tokio::time::timeout(
            Duration::from_secs(30),
            image_uploads::ensure_s3_temp_image_lifecycle(&http, &s3_cfg),
        )
        .await
        {
            Ok(Ok(true)) => {
                eprintln!(
                    "  S3 lifecycle: configured {}-day expiration for prefix '{}'",
                    s3_cfg.lifecycle_days, s3_cfg.prefix
                );
            }
            Ok(Ok(false)) => {
                eprintln!(
                    "  S3 lifecycle: verified {}-day expiration for prefix '{}'",
                    s3_cfg.lifecycle_days, s3_cfg.prefix
                );
            }
            Ok(Err(error)) => {
                eprintln!("WARNING: Failed to ensure S3 lifecycle rule: {error}");
            }
            Err(_) => {
                eprintln!("WARNING: Timed out ensuring S3 lifecycle rule");
            }
        }
    }

    let state = Arc::new(AppState {
        config: std::sync::Mutex::new(Arc::new(config)),
        http,
        sessions,
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: shutdown.clone(),
        shutdown_token,
        upload_token,
        hooks,
        memory_queue,
    });

    // Ensure main session exists (load from disk or create fresh)
    {
        let main_session = load_session_from_disk(MAIN_SESSION_ID).unwrap_or_else(|| {
            let mut s = Session::new_with_id(MAIN_SESSION_ID, "Main");
            let config = state.config();
            let model = s.effective_model(&config.model).to_string();
            let sys = build_system_prompt(&config, &s.workspace, &model, &s.disabled_system_skills);
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
        .route("/api/client-config", get(api_client_config))
        .route("/api/sessions", get(api_sessions))
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
        .layer(middleware::from_fn(enforce_local_request))
        .with_state(state.clone());

    let addr = format!("127.0.0.1:{port}");
    println!("🦀 LingClaw v2 listening on http://{addr}");
    println!(
        "   Tools: think, exec, read_file, write_file, patch_file, list_dir, search_files, http_fetch"
    );

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("Failed to bind {addr}: {error}");
            return;
        }
    };

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
#[path = "tests/main_tests.rs"]
mod main_tests;
