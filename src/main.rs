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
    message_token_len_for_provider, update_session_token_usage,
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
use hooks::{build_compressed_messages, find_auto_compress_cutoff};
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
            disabled_system_skills: HashSet::new(),
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
    config: Config,
    http: Client,
    sessions: Mutex<HashMap<String, Session>>,
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
    /// Currently active sub-agent task (set on `task_started`, cleared on terminal).
    active_task: Option<LiveTaskState>,
}

#[derive(Clone)]
struct LiveTaskState {
    agent: String,
    prompt: String,
    /// Latest cycle/phase from `task_progress` events.
    current_cycle: Option<usize>,
    current_phase: Option<String>,
    /// Tool calls reported via `task_tool` events (for replay on reconnect).
    tools: Vec<LiveTaskToolState>,
}

#[derive(Clone)]
struct LiveTaskToolState {
    tool: String,
    id: String,
}

/// Cap for replay buffer strings (128 KB). Keeps memory bounded for long outputs.
const LIVE_REPLAY_CAP: usize = 128 * 1024;
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
Do not call file tools just to verify or re-read BOOTSTRAP.md, AGENTS.md, AGENT.md, IDENTITY.md, USER.md, SOUL.md, or MEMORY.md when their content is already present below.\n\
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
                        active_task: None,
                    },
                );
            }
            "delta" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(content) = event["content"].as_str()
                    && round.assistant_text.len() < LIVE_REPLAY_CAP
                {
                    round.assistant_text.push_str(content);
                    round.assistant_text.truncate(LIVE_REPLAY_CAP);
                }
            }
            "thinking_start" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                {
                    round.reasoning_text.clear();
                    round.reasoning_done = false;
                }
            }
            "thinking_delta" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(content) = event["content"].as_str()
                    && round.reasoning_text.len() < LIVE_REPLAY_CAP
                {
                    round.reasoning_text.push_str(content);
                    round.reasoning_text.truncate(LIVE_REPLAY_CAP);
                }
            }
            "thinking_done" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                {
                    round.reasoning_done = true;
                }
            }
            "tool_call" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
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
                {
                    round.active_task = Some(LiveTaskState {
                        agent: event["agent"].as_str().unwrap_or_default().to_string(),
                        prompt: event["prompt"].as_str().unwrap_or_default().to_string(),
                        current_cycle: None,
                        current_phase: None,
                        tools: Vec::new(),
                    });
                }
            }
            "task_progress" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(task) = round.active_task.as_mut()
                {
                    task.current_cycle = event["cycle"].as_u64().map(|v| v as usize);
                    task.current_phase = event["phase"].as_str().map(str::to_string);
                }
            }
            "task_tool" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                    && let Some(task) = round.active_task.as_mut()
                {
                    task.tools.push(LiveTaskToolState {
                        tool: event["tool"].as_str().unwrap_or_default().to_string(),
                        id: event["id"].as_str().unwrap_or_default().to_string(),
                    });
                }
            }
            "task_completed" | "task_failed" => {
                if let Some(round) = live_rounds.get_mut(session_id)
                    && round.connection_id == connection_id
                {
                    round.active_task = None;
                }
            }
            "done" | "error" => {
                if live_rounds.get(session_id).map(|r| r.connection_id) == Some(connection_id) {
                    live_rounds.remove(session_id);
                }
            }
            _ => {}
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

    // Replay active sub-agent task if one is running.
    if let Some(task) = &live_round.active_task {
        ws_send(
            tx,
            &json!({
                "type": "task_started",
                "agent": task.agent,
                "prompt": task.prompt,
            }),
        )
        .await;

        // Replay latest progress (cycle/phase).
        if let Some(cycle) = task.current_cycle {
            ws_send(
                tx,
                &json!({
                    "type": "task_progress",
                    "agent": task.agent,
                    "cycle": cycle,
                    "phase": task.current_phase.as_deref().unwrap_or("analyze"),
                }),
            )
            .await;
        }

        // Replay tool calls reported by the sub-agent.
        for tool in &task.tools {
            ws_send(
                tx,
                &json!({
                    "type": "task_tool",
                    "agent": task.agent,
                    "tool": tool.tool,
                    "id": tool.id,
                }),
            )
            .await;
        }
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
    Json(json!({
        "status": "ok",
        "version": VERSION,
        "model": state.config.model,
        "sessions": sessions.len(),
    }))
}

async fn api_sessions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sessions = state.sessions.lock().await;
    let list: Vec<serde_json::Value> = sessions
        .get(MAIN_SESSION_ID)
        .map(|s| {
            json!({
                "id": MAIN_SESSION_ID,
                "name": s.name,
                "messages": s.messages.len(),
                "tool_calls": s.tool_calls_count,
                "model": s.effective_model(&state.config.model),
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

    let s3_cfg = state.config.s3.as_ref().ok_or_else(|| {
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

    let memory_queue = if config.structured_memory {
        Some(MemoryUpdateQueue::spawn(config.clone()))
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
        config,
        http,
        sessions: Mutex::new(HashMap::new()),
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
            let model = s.effective_model(&state.config.model).to_string();
            let sys = build_system_prompt(
                &state.config,
                &s.workspace,
                &model,
                &s.disabled_system_skills,
            );
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
