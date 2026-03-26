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
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{mpsc, Mutex};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;
use tower_http::services::ServeDir;

mod agent;
mod cli;
mod commands;
mod config;
mod context;
mod hooks;
mod prompts;
mod providers;
mod session_admin;
mod session_store;
mod socket_sync;
mod socket_tasks;
mod tools;

pub(crate) use config::{config_dir_path, config_file_path, Config, Provider, DEFAULT_PORT};
pub(crate) use context::{
    accumulate_daily_token_usage, context_input_budget_for_model, current_daily_token_usage,
    estimate_tokens_for_provider, format_token_count, format_usage_block,
    message_token_len_for_provider, prune_messages, update_session_token_usage,
};
pub(crate) use hooks::{run_hooks, AutoCompressContextHook, HookRegistry};

use commands::handle_command;
use session_admin::{
    admin_tool_definitions_anthropic, admin_tool_definitions_openai, execute_admin_tool,
    is_admin_tool,
};
use session_store::{
    list_recoverable_saved_session_ids, load_session_from_disk, refresh_session_system_prompt,
    save_session_to_disk,
};
use socket_sync::{
    send_command_refresh, send_existing_session_payloads, send_new_session_payload,
    send_session_switched_payloads, send_sessions_list,
};
use socket_tasks::{finalize_connection, spawn_connection_tasks, ConnectionCleanup};

#[cfg(test)]
use config::{JsonConfig, JsonModelEntry, JsonProviderConfig};
#[cfg(test)]
use context::{estimate_tokens, message_token_len, turn_len};
#[cfg(test)]
use hooks::{build_compressed_messages, find_auto_compress_cutoff};
#[cfg(test)]
use session_admin::{delete_session_by_id, gather_global_today_usage, gather_sessions_status};
#[cfg(test)]
use session_store::{
    build_active_session_lines, build_global_today_usage, build_history_payload,
    build_session_status, build_session_usage, build_usage_report, list_saved_session_ids_in_dir,
    list_saved_session_summaries_in_dir, recoverable_session_ids_from_summaries,
    resolve_session_target, sanitize_session_messages, sessions_dir, trim_incomplete_tool_calls,
};
#[cfg(test)]
use std::collections::HashSet;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const MAIN_SESSION_ID: &str = "main";

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
}

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
    fn new() -> Self {
        Self::new_with_id(&gen_session_id(), "New Chat")
    }

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
            model_override: None,
            think_level: default_think_level(),
            show_react: default_show_react(),
            show_tools: default_show_tools(),
            show_reasoning: default_show_reasoning(),
            version: SESSION_VERSION,
            workspace,
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
    /// Per-session active agent run cancellation tokens.
    active_runs: Mutex<HashMap<String, CancellationToken>>,
    next_connection_id: AtomicU64,
    shutdown: CancellationToken,
    shutdown_token: String,
    hooks: HookRegistry,
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
    elapsed_ms: u64,
}

#[derive(Clone, Default)]
struct LiveRoundState {
    round: usize,
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
const TOOL_PROGRESS_HEARTBEAT_SECS: u64 = 1;

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
    let mcp_note = tools::mcp::runtime_tool_note(config)
        .map(|note| format!("\n\n## MCP Runtime\n- {note}"))
        .unwrap_or_default();

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
{tool_lines}{admin_section}{mcp_note}"#,
        model = model,
        local_time = local_time,
        tool_lines = tool_lines,
        persona = persona,
        prompt_file_note = prompt_file_note,
        admin_section = admin_section,
        mcp_note = mcp_note,
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
                        elapsed_ms: 0,
                    });
                }
            }
            "tool_progress" => {
                if let Some(round) = live_rounds.get_mut(session_id) {
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
                if let Some(round) = live_rounds.get_mut(session_id) {
                    let tool_id = event["id"].as_str().unwrap_or_default();
                    let mut result = event["result"].as_str().unwrap_or_default().to_string();
                    result.truncate(LIVE_REPLAY_CAP);
                    if let Some(tool) = round.tools.iter_mut().find(|tool| tool.id == tool_id) {
                        tool.result = Some(result);
                        tool.elapsed_ms = event["duration_ms"].as_u64().unwrap_or(tool.elapsed_ms);
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

// ── Tool Dispatch ────────────────────────────────────────────────────────────

async fn execute_tool(
    name: &str,
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
) -> tools::ToolOutcome {
    if let Some(result) = tools::mcp::execute_tool(name, args_str, config, workspace).await {
        result
    } else {
        tools::execute_tool(name, args_str, config, http, workspace).await
    }
}

enum ToolRunState {
    Completed(tools::ToolOutcome),
    Abort,
}

async fn run_tool_with_feedback<F>(
    live_tx: &LiveTx,
    cancel: &CancellationToken,
    tool_id: &str,
    tool_name: &str,
    timeout: Duration,
    future: F,
) -> ToolRunState
where
    F: std::future::Future<Output = tools::ToolOutcome>,
{
    let start = std::time::Instant::now();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(TOOL_PROGRESS_HEARTBEAT_SECS));
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Skip);
    heartbeat.tick().await;

    let timeout_secs = timeout.as_secs();
    let sleep = tokio::time::sleep(timeout);
    tokio::pin!(sleep);
    tokio::pin!(future);

    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return ToolRunState::Abort;
            }
            _ = &mut sleep => {
                return ToolRunState::Completed(tools::ToolOutcome {
                    output: format!("{tool_name} error: tool execution timed out ({}s)", timeout_secs),
                    is_error: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                });
            }
            _ = heartbeat.tick() => {
                if !live_send(
                    live_tx,
                    json!({
                        "type": "tool_progress",
                        "id": tool_id,
                        "name": tool_name,
                        "elapsed_ms": start.elapsed().as_millis() as u64,
                    }),
                )
                .await
                {
                    return ToolRunState::Abort;
                }
            }
            result = &mut future => {
                return ToolRunState::Completed(result);
            }
        }
    }
}

// ── Session Claim ────────────────────────────────────────────────────────────

enum ClaimSessionResult {
    Claimed(String),
    InUse,
    NotFound,
}

/// Lock ordering: active_connections → session_clients → sessions.
/// All callers must acquire locks in this order to prevent deadlocks.
async fn try_claim_session(id: &str, state: &AppState, connection_id: u64) -> ClaimSessionResult {
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return ClaimSessionResult::NotFound;
    }

    // Phase 1: quick check — is there an active connection?
    if state.active_connections.lock().await.contains_key(id) {
        return ClaimSessionResult::InUse;
    }
    if state.session_clients.lock().await.contains_key(id) {
        return ClaimSessionResult::InUse;
    }

    // Phase 2: try claiming from in-memory orphan (no disk I/O)
    {
        let mut active = state.active_connections.lock().await;
        if active.contains_key(id) {
            return ClaimSessionResult::InUse;
        }
        if state.session_clients.lock().await.contains_key(id) {
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
    if state.session_clients.lock().await.contains_key(id) {
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
    let (inbound_tx, mut inbound_rx) = mpsc::unbounded_channel::<String>();
    let reader_cancel = connection_cancel.clone();
    let reader = tokio::spawn(async move {
        while let Some(result) = rx.next().await {
            match result {
                Ok(WsMsg::Text(t)) => {
                    // Use an unbounded inbound channel so /stop and intervention
                    // messages are not silently dropped while the agent is busy.
                    if inbound_tx.send(t.to_string()).is_err() {
                        break;
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
                let saved_ids = list_recoverable_saved_session_ids();

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
        send_existing_session_payloads(&tx, &state, &id).await;
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
        if let Err(error) = save_session_to_disk(&session).await {
            eprintln!(
                "Warning: failed to persist new session {} on creation: {error}; keeping in memory",
                current_session_id
            );
        }
        {
            let mut active = state.active_connections.lock().await;
            let mut sessions = state.sessions.lock().await;
            sessions.insert(current_session_id.clone(), session);
            active.insert(current_session_id.clone(), connection_id);
        }
        send_new_session_payload(&tx, &state, &current_session_id).await;
    }

    bind_session_connection(&state, &current_session_id, connection_id, &tx, false).await;
    replay_live_round(&tx, &state, &current_session_id).await;
    finish_session_replay(&state, &current_session_id, connection_id).await;
    send_sessions_list(&tx, &state, &current_session_id).await;

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
                    send_command_refresh(&tx, &state, &current_session_id, result.refresh_history)
                        .await;

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
                        bind_session_connection(
                            &state,
                            &current_session_id,
                            connection_id,
                            &tx,
                            true,
                        )
                        .await;
                        {
                            let mut active_id = current_session_ref.lock().await;
                            *active_id = current_session_id.clone();
                        }
                        send_session_switched_payloads(&tx, &state, &new_id).await;
                    }
                    if result.sessions_changed {
                        send_sessions_list(&tx, &state, &current_session_id).await;
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
        } // end if !rerun_agent
        rerun_agent = false;

        let show_react = {
            let sessions = state.sessions.lock().await;
            sessions
                .get(&current_session_id)
                .map(|s| s.show_react)
                .unwrap_or(false)
        };

        let mut shutting_down = false;
        let mut run_stopped = false;
        let mut round: usize = 0;
        let mut react_ctx = agent::AgentLoopCtx::new(show_react);
        const AGENT_HARD_CAP_ROUNDS: usize = 200;

        // Per-run cancellation: child of shutdown so server stop propagates automatically.
        let run_cancel = cancel.child_token();
        {
            let mut runs = state.active_runs.lock().await;
            runs.insert(current_session_id.clone(), run_cancel.clone());
        }

        // Inter-phase state: Analyze → Act → Observe
        let mut pending_tool_calls: Vec<ToolCall> = Vec::new();
        let mut collected_results: Vec<agent::ToolResultEntry> = Vec::new();
        let mut cycle_workspace = std::path::PathBuf::new();
        let mut cycle_is_main = false;
        let mut last_observation_hint: Option<String> = None;
        let mut pending_interventions: Vec<String> = Vec::new();

        'agent: loop {
            if cancel.is_cancelled() {
                shutting_down = true;
                break;
            }
            // Drain pending inbound messages — handle /stop and collect interventions
            while let Ok(msg) = inbound_rx.try_recv() {
                let m = msg.trim();
                if m.eq_ignore_ascii_case("/stop") {
                    run_cancel.cancel();
                    run_stopped = true;
                    break 'agent;
                }
                // Non-command text from user → queue as intervention for next Analyze
                if !m.is_empty() && !m.starts_with('/') {
                    pending_interventions.push(m.to_string());
                    let _ = live_send(
                        &live_tx,
                        json!({"type":"progress","content":"📝 Intervention received — will apply at next reasoning cycle"}),
                    )
                    .await;
                }
            }
            if run_cancel.is_cancelled() && !cancel.is_cancelled() {
                run_stopped = true;
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

                    // ── Inject pending user interventions as messages ──
                    if !pending_interventions.is_empty() {
                        let mut sessions = state.sessions.lock().await;
                        if let Some(session) = sessions.get_mut(&current_session_id) {
                            for text in pending_interventions.drain(..) {
                                session.messages.push(ChatMessage {
                                    role: "user".into(),
                                    content: Some(text),
                                    tool_calls: None,
                                    tool_call_id: None,
                                    timestamp: Some(now_epoch()),
                                });
                            }
                            session.updated_at = now_epoch();
                        }
                    }

                    // ── BeforeAnalyze hooks (e.g. auto-compress context) ──
                    let mut before_analyze_events = run_hooks(
                        &state.hooks,
                        agent::HookPoint::BeforeAnalyze,
                        &state.sessions,
                        &current_session_id,
                        &state.config,
                        &state.http,
                        react_ctx.cycles,
                    )
                    .await;

                    let (msgs_snapshot, model, workspace, think_level, pruned_count) = {
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
                            context_input_budget_for_model(&state.config, &model_str),
                        );
                        let pruned = msg_count_before - session.messages.len();
                        (
                            session.messages.clone(),
                            model_str,
                            session.workspace.clone(),
                            session.think_level.clone(),
                            pruned,
                        )
                    };

                    let final_context_estimate = estimate_tokens_for_provider(
                        state.config.resolve_model(&model).provider,
                        &msgs_snapshot,
                    );
                    for event in &mut before_analyze_events {
                        if event["type"] == "context_compressed" {
                            event["after_estimate"] = json!(final_context_estimate);
                        }
                    }

                    for event in before_analyze_events {
                        if !live_send(&live_tx, event).await {
                            break 'agent;
                        }
                    }

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

                    if !live_send(
                        &live_tx,
                        json!({
                            "type":"start",
                            "round": round + 1,
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
                    let mut extra_tools = extra_tools;
                    let mut mcp_tools = match resolved.provider {
                        Provider::Anthropic => {
                            tools::mcp::tool_definitions_anthropic(&state.config, &cycle_workspace)
                                .await
                        }
                        Provider::OpenAI => {
                            tools::mcp::tool_definitions_openai(&state.config, &cycle_workspace)
                                .await
                        }
                    };
                    extra_tools.append(&mut mcp_tools);

                    let llm_result = tokio::select! {
                        biased;
                        _ = run_cancel.cancelled() => {
                            shutting_down = cancel.is_cancelled();
                            run_stopped = !shutting_down;
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
                            let input_tokens = resp.input_tokens.unwrap_or_else(|| {
                                estimate_tokens_for_provider(resolved.provider, &msgs_snapshot)
                                    as u64
                            });
                            let output_tokens = resp.output_tokens.unwrap_or_else(|| {
                                message_token_len_for_provider(resolved.provider, &resp.message)
                                    as u64
                            });
                            let has_content = resp.message.has_nonempty_content();
                            let has_tools = resp.message.has_tool_calls();
                            let should_persist = !resp.message.is_empty_assistant_message();

                            {
                                let mut sessions = state.sessions.lock().await;
                                if let Some(session) = sessions.get_mut(&current_session_id) {
                                    let input_source = if resp.input_tokens.is_some() {
                                        "provider"
                                    } else {
                                        "estimated"
                                    };
                                    let output_source = if resp.output_tokens.is_some() {
                                        "provider"
                                    } else {
                                        "estimated"
                                    };
                                    update_session_token_usage(
                                        session,
                                        input_tokens,
                                        output_tokens,
                                        input_source,
                                        output_source,
                                    );
                                }
                            }

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
                    let tool_timeout = state.config.tool_timeout;

                    for tc in &pending_tool_calls {
                        if run_cancel.is_cancelled() {
                            shutting_down = cancel.is_cancelled();
                            run_stopped = !shutting_down;
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

                        let run_state = if cycle_is_main && is_admin_tool(&tc.function.name) {
                            run_tool_with_feedback(
                                &live_tx,
                                &run_cancel,
                                &tc.id,
                                &tc.function.name,
                                tool_timeout,
                                async {
                                    let start = std::time::Instant::now();
                                    let output = execute_admin_tool(
                                        &tc.function.name,
                                        &tc.function.arguments,
                                        &state,
                                    )
                                    .await;
                                    let duration_ms = start.elapsed().as_millis() as u64;
                                    let is_error =
                                        tools::is_tool_error_output(&tc.function.name, &output);
                                    tools::ToolOutcome {
                                        output,
                                        is_error,
                                        duration_ms,
                                    }
                                },
                            )
                            .await
                        } else {
                            run_tool_with_feedback(
                                &live_tx,
                                &run_cancel,
                                &tc.id,
                                &tc.function.name,
                                tool_timeout,
                                execute_tool(
                                    &tc.function.name,
                                    &tc.function.arguments,
                                    &state.config,
                                    &state.http,
                                    &cycle_workspace,
                                ),
                            )
                            .await
                        };

                        let result = match run_state {
                            ToolRunState::Completed(result) => result,
                            ToolRunState::Abort => {
                                shutting_down = cancel.is_cancelled();
                                run_stopped = !shutting_down;
                                break 'agent;
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

                    // ── AfterObserve hooks ──
                    let after_observe_events = run_hooks(
                        &state.hooks,
                        agent::HookPoint::AfterObserve,
                        &state.sessions,
                        &current_session_id,
                        &state.config,
                        &state.http,
                        react_ctx.cycles,
                    )
                    .await;

                    for event in after_observe_events {
                        let _ = live_send(&live_tx, event).await;
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

                    // ── OnFinish hooks ──
                    let on_finish_events = run_hooks(
                        &state.hooks,
                        agent::HookPoint::OnFinish,
                        &state.sessions,
                        &current_session_id,
                        &state.config,
                        &state.http,
                        react_ctx.cycles,
                    )
                    .await;

                    for event in on_finish_events {
                        let _ = live_send(&live_tx, event).await;
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

        // Clean up per-run cancellation token
        {
            let mut runs = state.active_runs.lock().await;
            runs.remove(&current_session_id);
        }

        // If the agent finished normally but there are pending interventions
        // that arrived during/after the last cycle, persist them and re-run
        // the agent so the user's message gets a response.
        if !run_stopped && !shutting_down && !pending_interventions.is_empty() {
            {
                let mut sessions = state.sessions.lock().await;
                if let Some(session) = sessions.get_mut(&current_session_id) {
                    for text in pending_interventions.drain(..) {
                        session.messages.push(ChatMessage {
                            role: "user".into(),
                            content: Some(text),
                            tool_calls: None,
                            tool_call_id: None,
                            timestamp: Some(now_epoch()),
                        });
                    }
                    session.updated_at = now_epoch();
                }
            }
            rerun_agent = true;
        }

        if run_stopped {
            // Persist any pending interventions so they survive in history
            if !pending_interventions.is_empty() {
                let mut sessions = state.sessions.lock().await;
                if let Some(session) = sessions.get_mut(&current_session_id) {
                    for text in pending_interventions.drain(..) {
                        session.messages.push(ChatMessage {
                            role: "user".into(),
                            content: Some(text),
                            tool_calls: None,
                            tool_call_id: None,
                            timestamp: Some(now_epoch()),
                        });
                    }
                }
            }
            // Trim incomplete tool calls from session history
            {
                let mut sessions = state.sessions.lock().await;
                if let Some(session) = sessions.get_mut(&current_session_id) {
                    session_store::trim_incomplete_tool_calls(&mut session.messages);
                }
            }
            let _ = live_send(
                &live_tx,
                json!({"type":"done","phase":"stopped","reason":"user_stop","cycles":react_ctx.cycles,"tool_calls":react_ctx.tool_calls}),
            )
            .await;
        }

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

/// Wait for a specific session to become available (old connection releasing it),
/// then load from disk and claim it. Returns None if unavailable after timeout.
async fn claim_requested_session(id: &str, state: &AppState, connection_id: u64) -> Option<String> {
    // Validate ID format
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return None;
    }
    // Browser refresh can race the old socket disconnect: the new connection may arrive
    // while the previous client binding still exists for the same session. Give that
    // binding a short grace window to disappear before treating it as a different live client.
    const REFRESH_CLIENT_GRACE_POLLS: usize = 6;

    // Wait up to 30 seconds for an in-flight generation round to finish and release the session.
    for attempt in 0..60 {
        let active = state.active_connections.lock().await.contains_key(id);
        let has_bound_client = state.session_clients.lock().await.contains_key(id);
        if !active && !has_bound_client {
            break;
        }
        if has_bound_client && attempt >= REFRESH_CLIENT_GRACE_POLLS {
            // Another client is still actively bound after the refresh grace window.
            return None;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    match try_claim_session(id, state, connection_id).await {
        ClaimSessionResult::Claimed(id) => Some(id),
        ClaimSessionResult::InUse | ClaimSessionResult::NotFound => None,
    }
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
        "  Context limit: {} tokens",
        config.context_limit_for_model(&config.model)
    );

    let shutdown = CancellationToken::new();

    // Generate a one-time shutdown token and write it to disk for CLI use
    let shutdown_token = generate_shutdown_token();
    if let Some(dir) = config_dir_path() {
        let _ = std::fs::write(dir.join(format!("shutdown-{port}.token")), &shutdown_token);
    }

    let mut hooks = HookRegistry::new();
    hooks.register(Box::new(AutoCompressContextHook::new()));

    let state = Arc::new(AppState {
        config,
        http: Client::new(),
        sessions: Mutex::new(HashMap::new()),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: shutdown.clone(),
        shutdown_token,
        hooks,
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
#[path = "tests/main_tests.rs"]
mod main_tests;
