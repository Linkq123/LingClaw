use futures::future::join_all;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex as AsyncMutex,
    task::JoinHandle,
};

use crate::{Config, VERSION, config::JsonMcpServerConfig, resolve_path_checked};

use super::ToolOutcome;

const MCP_NAME_PREFIX: &str = "mcp__";
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_DIAGNOSTIC_LINE_LIMIT: usize = 6;
const MCP_DIAGNOSTIC_CHAR_LIMIT: usize = 400;
const MCP_TOOL_CACHE_TTL_SECS: u64 = 30;
const MCP_SESSION_IDLE_TTL_SECS: u64 = 300;
const MCP_SPAWN_FAILURE_COOLDOWN_SECS: u64 = 15;
static MCP_TOOL_CACHE: OnceLock<Mutex<HashMap<String, CachedToolDescriptors>>> = OnceLock::new();
static MCP_SESSION_CACHE: OnceLock<Mutex<HashMap<String, CachedMcpSession>>> = OnceLock::new();
static MCP_SPAWN_FAILURES: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();

#[derive(Clone, Debug)]
struct McpToolDescriptor {
    server_name: String,
    raw_name: String,
    exposed_name: String,
    description: String,
    input_schema: Value,
}

#[derive(Clone, Debug)]
struct CachedToolDescriptors {
    descriptors: Vec<McpToolDescriptor>,
    loaded_at: Instant,
}

#[derive(Clone, Debug)]
pub(crate) struct McpServerLoadReport {
    pub(crate) server_name: String,
    pub(crate) tool_names: Vec<String>,
    pub(crate) error: Option<String>,
}

struct CachedMcpSession {
    session: Arc<AsyncMutex<McpServerSession>>,
    last_used_at: Instant,
}

struct McpServerSession {
    server_name: String,
    workspace_root: PathBuf,
    tool_cache_key: String,
    timeout_secs: u64,
    next_request_id: u64,
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    stderr_task: JoinHandle<()>,
    stdout_lines: Arc<Mutex<Vec<String>>>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
}

pub(crate) fn runtime_tool_note(config: &Config) -> Option<String> {
    let mut names: Vec<&str> = config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, _)| name.as_str())
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort_unstable();
    Some(format!(
        "Additional MCP tools may be injected at runtime from configured MCP servers: {}. MCP tool names are prefixed with 'mcp__'.",
        names.join(", ")
    ))
}

pub(crate) async fn tool_definitions_openai(config: &Config, workspace: &Path) -> Vec<Value> {
    list_tools(config, workspace)
        .await
        .into_iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.exposed_name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })
        })
        .collect()
}

pub(crate) fn cached_tool_definitions_openai(config: &Config, workspace: &Path) -> Vec<Value> {
    cached_list_tools(config, workspace)
        .into_iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.exposed_name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                }
            })
        })
        .collect()
}

pub(crate) async fn tool_definitions_ollama(config: &Config, workspace: &Path) -> Vec<Value> {
    tool_definitions_openai(config, workspace).await
}

pub(crate) fn cached_tool_definitions_ollama(config: &Config, workspace: &Path) -> Vec<Value> {
    cached_tool_definitions_openai(config, workspace)
}

pub(crate) async fn tool_definitions_anthropic(config: &Config, workspace: &Path) -> Vec<Value> {
    list_tools(config, workspace)
        .await
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.exposed_name,
                "description": tool.description,
                "input_schema": tool.input_schema,
            })
        })
        .collect()
}

pub(crate) fn cached_tool_definitions_anthropic(config: &Config, workspace: &Path) -> Vec<Value> {
    cached_list_tools(config, workspace)
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.exposed_name,
                "description": tool.description,
                "input_schema": tool.input_schema,
            })
        })
        .collect()
}

pub(crate) fn cached_server_counts(config: &Config, workspace: &Path) -> (usize, usize) {
    let mut enabled_servers = 0;
    let mut cached_servers = 0;
    let now = Instant::now();

    for (server_name, server) in config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
    {
        enabled_servers += 1;
        let Ok(key) = cache_key(server_name, server, workspace, config) else {
            continue;
        };
        let has_cache = {
            let Ok(mut cache) = tool_cache().lock() else {
                continue;
            };
            match cache.get(&key) {
                Some(entry) if now.duration_since(entry.loaded_at) < tool_cache_ttl() => true,
                Some(_) => {
                    cache.remove(&key);
                    false
                }
                None => false,
            }
        };
        if has_cache {
            cached_servers += 1;
        }
    }

    (cached_servers, enabled_servers)
}

pub(crate) async fn execute_tool(
    name: &str,
    args_str: &str,
    config: &Config,
    workspace: &Path,
) -> Option<ToolOutcome> {
    if !name.starts_with(MCP_NAME_PREFIX) {
        return None;
    }

    let start = Instant::now();
    let args: Value = match serde_json::from_str(args_str) {
        Ok(value) => value,
        Err(error) => {
            return Some(ToolOutcome {
                output: format!("{name} error: invalid arguments JSON: {error}"),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }
    };

    let descriptor = match find_tool_by_exposed_name(name, config, workspace).await {
        Ok(Some(tool)) => tool,
        Ok(None) => {
            return Some(ToolOutcome {
                output: format!("Unknown MCP tool: {name}"),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }
        Err(error) => {
            return Some(ToolOutcome {
                output: format!("{name} error: {error}"),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }
    };

    let call_result = call_server(
        &descriptor.server_name,
        config,
        workspace,
        "tools/call",
        json!({
            "name": descriptor.raw_name,
            "arguments": args,
        }),
    )
    .await;

    let duration_ms = start.elapsed().as_millis() as u64;
    match call_result {
        Ok(result) => {
            let is_error = result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(ToolOutcome {
                output: render_call_result(&result),
                is_error,
                duration_ms,
            })
        }
        Err(error) => Some(ToolOutcome {
            output: format!("{name} error: {error}"),
            is_error: true,
            duration_ms,
        }),
    }
}

pub(crate) async fn inspect_servers(config: &Config, workspace: &Path) -> Vec<McpServerLoadReport> {
    let mut server_names: Vec<&str> = config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, _)| name.as_str())
        .collect();
    server_names.sort_unstable();

    join_all(server_names.into_iter().map(|server_name| async move {
        match list_server_tools_uncached(server_name, config, workspace).await {
            Ok(tools) => McpServerLoadReport {
                server_name: server_name.to_string(),
                tool_names: tools.into_iter().map(|tool| tool.exposed_name).collect(),
                error: None,
            },
            Err(error) => McpServerLoadReport {
                server_name: server_name.to_string(),
                tool_names: Vec::new(),
                error: Some(error),
            },
        }
    }))
    .await
}

pub(crate) async fn refresh_servers(
    config: &Config,
    workspace: &Path,
) -> Result<Vec<McpServerLoadReport>, String> {
    refresh_server_caches(config, workspace).await?;

    let mut server_names: Vec<&str> = config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, _)| name.as_str())
        .collect();
    server_names.sort_unstable();

    let results = join_all(server_names.into_iter().map(|server_name| async move {
        match list_server_tools_uncached(server_name, config, workspace).await {
            Ok(tools) => {
                let server = config
                    .mcp_servers
                    .get(server_name)
                    .ok_or_else(|| format!("unknown MCP server '{server_name}'"));
                match server.and_then(|server| cache_key(server_name, server, workspace, config)) {
                    Ok(cache_key) => {
                        // Lock scope is synchronous — no .await while held.
                        let mut cache = tool_cache()
                            .lock()
                            .map_err(|_| "MCP tool cache lock poisoned".to_string())?;
                        cache.insert(
                            cache_key,
                            CachedToolDescriptors {
                                descriptors: tools.clone(),
                                loaded_at: Instant::now(),
                            },
                        );
                        Ok(McpServerLoadReport {
                            server_name: server_name.to_string(),
                            tool_names: tools.into_iter().map(|tool| tool.exposed_name).collect(),
                            error: None,
                        })
                    }
                    Err(error) => Ok(McpServerLoadReport {
                        server_name: server_name.to_string(),
                        tool_names: Vec::new(),
                        error: Some(error),
                    }),
                }
            }
            Err(error) => Ok(McpServerLoadReport {
                server_name: server_name.to_string(),
                tool_names: Vec::new(),
                error: Some(error),
            }),
        }
    }))
    .await;

    results.into_iter().collect()
}

fn tool_cache() -> &'static Mutex<HashMap<String, CachedToolDescriptors>> {
    MCP_TOOL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_cache() -> &'static Mutex<HashMap<String, CachedMcpSession>> {
    MCP_SESSION_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn stable_name_suffix(server_name: &str, tool_name: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in server_name
        .as_bytes()
        .iter()
        .chain([0xff].iter())
        .chain(tool_name.as_bytes().iter())
    {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", (hash & 0xffff_ffff) as u32)
}

fn sanitize_name_segment(raw: &str) -> String {
    let mut sanitized = String::new();
    let mut last_was_underscore = false;
    for ch in raw.chars() {
        let mapped = if ch.is_ascii_alphanumeric() { ch } else { '_' };
        if mapped == '_' {
            if last_was_underscore {
                continue;
            }
            last_was_underscore = true;
        } else {
            last_was_underscore = false;
        }
        sanitized.push(mapped.to_ascii_lowercase());
    }
    let trimmed = sanitized.trim_matches('_');
    let mut output = if trimmed.is_empty() {
        "tool".to_string()
    } else {
        trimmed.to_string()
    };
    if !output
        .chars()
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic())
    {
        output.insert(0, 't');
        output.insert(1, '_');
    }
    output
}

fn build_exposed_name(server_name: &str, tool_name: &str) -> String {
    let server = sanitize_name_segment(server_name);
    let tool = sanitize_name_segment(tool_name);
    let suffix = stable_name_suffix(server_name, tool_name);
    format!("{MCP_NAME_PREFIX}{server}__{tool}__{suffix}")
}

fn render_call_result(result: &Value) -> String {
    let mut parts = Vec::new();

    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for item in content {
            if let Some(text) = item.get("text").and_then(Value::as_str)
                && !text.is_empty()
            {
                parts.push(text.to_string());
                continue;
            }
            let item_type = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            parts.push(format!(
                "[{item_type}] {}",
                serde_json::to_string_pretty(item).unwrap_or_else(|_| item.to_string())
            ));
        }
    }

    if let Some(structured) = result.get("structuredContent") {
        parts.push(format!(
            "structuredContent:\n{}",
            serde_json::to_string_pretty(structured).unwrap_or_else(|_| structured.to_string())
        ));
    }

    if parts.is_empty() {
        serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string())
    } else {
        parts.join("\n\n")
    }
}

async fn list_tools(config: &Config, workspace: &Path) -> Vec<McpToolDescriptor> {
    let mut server_names: Vec<&str> = config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, _)| name.as_str())
        .collect();
    server_names.sort_unstable();

    let mut tools = Vec::new();
    let results = join_all(server_names.into_iter().map(|server_name| async move {
        (
            server_name,
            list_server_tools(server_name, config, workspace).await,
        )
    }))
    .await;

    for (server_name, result) in results {
        match result {
            Ok(mut server_tools) => tools.append(&mut server_tools),
            Err(error) => eprintln!("Warning: MCP server '{server_name}' unavailable: {error}"),
        }
    }
    tools
}

fn cached_list_tools(config: &Config, workspace: &Path) -> Vec<McpToolDescriptor> {
    let mut server_names: Vec<&str> = config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, _)| name.as_str())
        .collect();
    server_names.sort_unstable();

    let mut tools = Vec::new();
    let now = Instant::now();

    for server_name in server_names {
        let Some(server) = config.mcp_servers.get(server_name) else {
            continue;
        };
        let Ok(key) = cache_key(server_name, server, workspace, config) else {
            continue;
        };
        let cached = {
            let Ok(mut cache) = tool_cache().lock() else {
                continue;
            };
            match cache.get(&key) {
                Some(entry) if now.duration_since(entry.loaded_at) < tool_cache_ttl() => {
                    Some(entry.descriptors.clone())
                }
                Some(_) => {
                    cache.remove(&key);
                    None
                }
                None => None,
            }
        };

        if let Some(mut cached) = cached {
            tools.append(&mut cached);
        }
    }

    tools
}

fn cache_key(
    server_name: &str,
    server: &JsonMcpServerConfig,
    workspace: &Path,
    config: &Config,
) -> Result<String, String> {
    let resolved_cwd = resolve_server_cwd(server, workspace)?;
    let mut env_items: Vec<String> = server
        .env
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    env_items.sort_unstable();
    Ok(format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        server_name,
        server.command,
        server.args.join("\u{1f}"),
        resolved_cwd.display(),
        server_timeout_secs(server, config),
        env_items.join("\u{1f}")
    ))
}

async fn find_tool_by_exposed_name(
    name: &str,
    config: &Config,
    workspace: &Path,
) -> Result<Option<McpToolDescriptor>, String> {
    let Some(rest) = name.strip_prefix(MCP_NAME_PREFIX) else {
        return Ok(None);
    };
    let Some((server_segment, _)) = rest.split_once("__") else {
        return Ok(None);
    };

    let mut matching_servers: Vec<&str> = config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .filter(|(server_name, _)| sanitize_name_segment(server_name) == server_segment)
        .map(|(server_name, _)| server_name.as_str())
        .collect();
    matching_servers.sort_unstable();

    for server_name in matching_servers {
        let tools = list_server_tools(server_name, config, workspace).await?;
        if let Some(tool) = tools.into_iter().find(|tool| tool.exposed_name == name) {
            return Ok(Some(tool));
        }
    }

    Ok(None)
}

async fn list_server_tools(
    server_name: &str,
    config: &Config,
    workspace: &Path,
) -> Result<Vec<McpToolDescriptor>, String> {
    let server = config
        .mcp_servers
        .get(server_name)
        .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
    let key = cache_key(server_name, server, workspace, config)?;
    let now = Instant::now();

    let cached = {
        let mut cache = tool_cache()
            .lock()
            .map_err(|_| "MCP tool cache lock poisoned".to_string())?;
        match cache.get(&key) {
            Some(entry) if now.duration_since(entry.loaded_at) < tool_cache_ttl() => {
                Some(entry.descriptors.clone())
            }
            Some(_) => {
                cache.remove(&key);
                None
            }
            None => None,
        }
    };
    if let Some(cached) = cached {
        return Ok(cached);
    }

    let response = call_server(server_name, config, workspace, "tools/list", json!({})).await?;
    let descriptors = parse_tool_descriptors(server_name, &response)?;

    {
        let mut cache = tool_cache()
            .lock()
            .map_err(|_| "MCP tool cache lock poisoned".to_string())?;
        cache.insert(
            key,
            CachedToolDescriptors {
                descriptors: descriptors.clone(),
                loaded_at: Instant::now(),
            },
        );
    }

    Ok(descriptors)
}

async fn list_server_tools_uncached(
    server_name: &str,
    config: &Config,
    workspace: &Path,
) -> Result<Vec<McpToolDescriptor>, String> {
    let response =
        call_server_once(server_name, config, workspace, "tools/list", json!({})).await?;
    parse_tool_descriptors(server_name, &response)
}

fn parse_tool_descriptors(
    server_name: &str,
    response: &Value,
) -> Result<Vec<McpToolDescriptor>, String> {
    let tools = response
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("server '{server_name}' returned invalid tools/list payload"))?;

    let mut descriptors = Vec::with_capacity(tools.len());
    let mut seen = HashSet::new();
    for tool in tools {
        let raw_name = tool
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("server '{server_name}' returned tool without a name"))?;
        let exposed_name = build_exposed_name(server_name, raw_name);
        if !seen.insert(exposed_name.clone()) {
            return Err(format!(
                "server '{server_name}' exposes multiple tools that collide after name normalization"
            ));
        }
        let description = tool
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("MCP tool")
            .to_string();
        let input_schema = tool
            .get("inputSchema")
            .or_else(|| tool.get("input_schema"))
            .cloned()
            .unwrap_or_else(|| json!({"type":"object","properties":{},"required":[]}));
        descriptors.push(McpToolDescriptor {
            server_name: server_name.to_string(),
            raw_name: raw_name.to_string(),
            exposed_name,
            description,
            input_schema,
        });
    }

    Ok(descriptors)
}

fn server_timeout_secs(server: &JsonMcpServerConfig, config: &Config) -> u64 {
    server.timeout_secs.unwrap_or(config.tool_timeout.as_secs())
}

fn tool_cache_ttl() -> Duration {
    Duration::from_secs(MCP_TOOL_CACHE_TTL_SECS)
}

fn session_idle_ttl() -> Duration {
    Duration::from_secs(MCP_SESSION_IDLE_TTL_SECS)
}

fn spawn_failures() -> &'static Mutex<HashMap<String, Instant>> {
    MCP_SPAWN_FAILURES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn record_spawn_failure(server_name: &str) {
    if let Ok(mut map) = spawn_failures().lock() {
        map.insert(server_name.to_string(), Instant::now());
    }
}

fn check_spawn_cooldown(server_name: &str) -> Option<u64> {
    let map = spawn_failures().lock().ok()?;
    let last_failure = map.get(server_name)?;
    let elapsed = last_failure.elapsed();
    let cooldown = Duration::from_secs(MCP_SPAWN_FAILURE_COOLDOWN_SECS);
    if elapsed < cooldown {
        Some(cooldown.as_secs() - elapsed.as_secs())
    } else {
        None
    }
}

fn clear_spawn_failure(server_name: &str) {
    if let Ok(mut map) = spawn_failures().lock() {
        map.remove(server_name);
    }
}

fn resolve_server_command(command: &str) -> PathBuf {
    resolve_server_command_from_env(
        command,
        std::env::var_os("PATH"),
        std::env::var_os("HOME"),
        std::env::var_os("USERPROFILE"),
    )
}

fn resolve_server_command_from_env(
    command: &str,
    path_env: Option<OsString>,
    home_env: Option<OsString>,
    userprofile_env: Option<OsString>,
) -> PathBuf {
    let command_path = Path::new(command);
    if command_path.is_absolute() || command.contains(['/', '\\']) {
        return command_path.to_path_buf();
    }

    for dir in command_search_dirs(path_env, home_env, userprofile_env) {
        for candidate in command_candidates(&dir, command) {
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    command_path.to_path_buf()
}

fn command_search_dirs(
    path_env: Option<OsString>,
    home_env: Option<OsString>,
    userprofile_env: Option<OsString>,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(path) = path_env {
        dirs.extend(std::env::split_paths(&path));
    }

    let home_dir = home_env
        .map(PathBuf::from)
        .or_else(|| userprofile_env.map(PathBuf::from));
    if let Some(home_dir) = home_dir {
        dirs.push(home_dir.join(".local").join("bin"));
    }

    let mut seen = HashSet::new();
    dirs.retain(|dir| seen.insert(dir.clone()));
    dirs
}

fn command_candidates(dir: &Path, command: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let command_path = Path::new(command);
    if command_path.extension().is_some() {
        candidates.push(dir.join(command));
        return candidates;
    }

    candidates.push(dir.join(command));
    if cfg!(windows) {
        let pathext =
            std::env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
        for ext in pathext.to_string_lossy().split(';') {
            let trimmed = ext.trim();
            if trimmed.is_empty() {
                continue;
            }
            candidates.push(dir.join(format!("{command}{trimmed}")));
        }
    }

    candidates
}

fn resolve_server_cwd(server: &JsonMcpServerConfig, workspace: &Path) -> Result<PathBuf, String> {
    match server.cwd.as_deref() {
        Some(cwd) if !cwd.is_empty() => resolve_path_checked(cwd, workspace)
            .map_err(|message| format!("MCP server cwd '{}' is invalid: {message}", cwd)),
        _ => Ok(workspace.to_path_buf()),
    }
}

fn path_to_file_uri(path: &Path) -> String {
    let mut normalized = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) && !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }

    let mut encoded = String::new();
    for byte in normalized.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~' | '/' | ':') {
            encoded.push(ch);
        } else {
            encoded.push_str(&format!("%{:02X}", byte));
        }
    }

    format!("file://{encoded}")
}

fn workspace_roots_result(server_name: &str, workspace_root: &Path) -> Value {
    let name = workspace_root
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or(server_name);
    json!({
        "roots": [
            {
                "uri": path_to_file_uri(workspace_root),
                "name": name,
            }
        ]
    })
}

fn remove_cached_tool_descriptors(cache_key: &str) {
    if let Ok(mut cache) = tool_cache().lock() {
        cache.remove(cache_key);
    }
}

async fn remove_cached_sessions(cache_keys: &[String]) {
    let removed = {
        let Ok(mut cache) = session_cache().lock() else {
            return;
        };
        let mut removed = Vec::new();
        for cache_key in cache_keys {
            let removed_entry = cache.remove(cache_key);
            if let Some(entry) = removed_entry {
                removed.push(entry.session);
            }
        }
        removed
    };

    for session in removed {
        let mut guard = session.lock().await;
        guard.shutdown().await;
    }
}

async fn refresh_server_caches(config: &Config, workspace: &Path) -> Result<(), String> {
    let mut cache_keys = Vec::new();
    for (server_name, server) in config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
    {
        cache_keys.push(cache_key(server_name, server, workspace, config)?);
        clear_spawn_failure(server_name);
    }

    {
        let mut cache = tool_cache()
            .lock()
            .map_err(|_| "MCP tool cache lock poisoned".to_string())?;
        for cache_key in &cache_keys {
            cache.remove(cache_key);
        }
    }

    remove_cached_sessions(&cache_keys).await;
    Ok(())
}

fn push_diagnostic_line(lines: &mut Vec<String>, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    let mut clipped = trimmed.to_string();
    if clipped.len() > MCP_DIAGNOSTIC_CHAR_LIMIT {
        clipped.truncate(MCP_DIAGNOSTIC_CHAR_LIMIT);
        clipped.push_str("...");
    }

    if lines.len() == MCP_DIAGNOSTIC_LINE_LIMIT {
        lines.remove(0);
    }
    lines.push(clipped);
}

fn record_diagnostic_line(lines: &Arc<Mutex<Vec<String>>>, line: &str) {
    if let Ok(mut guard) = lines.lock() {
        push_diagnostic_line(&mut guard, line);
    }
}

fn snapshot_diagnostic_lines(lines: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    lines.lock().map(|guard| guard.clone()).unwrap_or_default()
}

fn format_mcp_diagnostics(stdout_lines: &[String], stderr_lines: &[String]) -> String {
    let mut parts = Vec::new();
    if !stdout_lines.is_empty() {
        parts.push(format!("stdout: {}", stdout_lines.join(" | ")));
    }
    if !stderr_lines.is_empty() {
        parts.push(format!("stderr: {}", stderr_lines.join(" | ")));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" [{}]", parts.join("; "))
    }
}

fn format_mcp_timeout_error(
    phase: &str,
    timeout_secs: u64,
    stdout_lines: &[String],
    stderr_lines: &[String],
) -> String {
    format!(
        "MCP {phase} timed out after {timeout_secs}s{}",
        format_mcp_diagnostics(stdout_lines, stderr_lines)
    )
}

async fn collect_stderr_lines(stderr: ChildStderr, lines: Arc<Mutex<Vec<String>>>) {
    let mut reader = BufReader::new(stderr);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => record_diagnostic_line(&lines, &line),
            Err(_) => break,
        }
    }
}

impl McpServerSession {
    async fn initialize(&mut self) -> Result<(), String> {
        write_message(
            &mut self.stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "LingClaw",
                        "version": VERSION,
                    }
                }
            }),
        )
        .await?;
        let initialize = tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            read_response(
                &mut self.reader,
                &mut self.stdin,
                1,
                &self.stdout_lines,
                &self.server_name,
                &self.workspace_root,
                &self.tool_cache_key,
            ),
        )
        .await
        .map_err(|_| {
            format_mcp_timeout_error(
                "initialize",
                self.timeout_secs,
                &snapshot_diagnostic_lines(&self.stdout_lines),
                &snapshot_diagnostic_lines(&self.stderr_lines),
            )
        })??;
        if let Some(error) = initialize.get("error") {
            return Err(format!(
                "initialize failed: {}",
                serde_json::to_string(error).unwrap_or_else(|_| error.to_string())
            ));
        }

        write_message(
            &mut self.stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
        )
        .await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let request_id = self.next_request_id;
        self.next_request_id += 1;

        write_message(
            &mut self.stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params,
            }),
        )
        .await?;

        let response = tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            read_response(
                &mut self.reader,
                &mut self.stdin,
                request_id,
                &self.stdout_lines,
                &self.server_name,
                &self.workspace_root,
                &self.tool_cache_key,
            ),
        )
        .await
        .map_err(|_| {
            format_mcp_timeout_error(
                method,
                self.timeout_secs,
                &snapshot_diagnostic_lines(&self.stdout_lines),
                &snapshot_diagnostic_lines(&self.stderr_lines),
            )
        })??;

        if let Some(error) = response.get("error") {
            return Err(serde_json::to_string(error).unwrap_or_else(|_| error.to_string()));
        }

        response
            .get("result")
            .cloned()
            .ok_or_else(|| format!("server response missing result for method '{method}'"))
    }

    fn decorate_error(&self, error: String) -> String {
        if error.contains("timed out after") || error.contains("initialize failed") {
            return error;
        }

        format!(
            "{error}{}",
            format_mcp_diagnostics(
                &snapshot_diagnostic_lines(&self.stdout_lines),
                &snapshot_diagnostic_lines(&self.stderr_lines),
            )
        )
    }

    async fn shutdown(&mut self) {
        let _ = self.stdin.shutdown().await;
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
        self.stderr_task.abort();
        let _ = (&mut self.stderr_task).await;
    }
}

fn should_reset_mcp_session(error: &str) -> bool {
    error.contains("timed out after")
        || error.contains("initialize failed")
        || error.contains("closed stdout")
        || error.contains("failed to spawn")
        || error.contains("missing stdin")
        || error.contains("missing stdout")
        || error.contains("missing stderr")
        || error.contains("invalid Content-Length")
        || error.contains("invalid MCP JSON")
        || error.contains("pipe")
}

async fn spawn_server_session(
    server_name: &str,
    config: &Config,
    workspace: &Path,
) -> Result<McpServerSession, String> {
    // Backoff: reject spawn if server recently failed.
    if let Some(remaining_secs) = check_spawn_cooldown(server_name) {
        return Err(format!(
            "MCP server '{server_name}' is in cooldown after recent failure ({remaining_secs}s remaining)"
        ));
    }

    let server = config
        .mcp_servers
        .get(server_name)
        .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
    let tool_cache_key = cache_key(server_name, server, workspace, config)?;
    let server_cwd = resolve_server_cwd(server, workspace)?;
    let resolved_command = resolve_server_command(&server.command);
    let mut command = Command::new(&resolved_command);
    command
        .args(&server.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .current_dir(server_cwd);
    for (key, value) in &server.env {
        command.env(key, value);
    }

    let stdout_lines = Arc::new(Mutex::new(Vec::new()));
    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to spawn '{}': {error}", server.command))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| format!("server '{server_name}' missing stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("server '{server_name}' missing stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("server '{server_name}' missing stderr"))?;
    let stderr_task = tokio::spawn(collect_stderr_lines(stderr, stderr_lines.clone()));
    let mut session = McpServerSession {
        server_name: server_name.to_string(),
        workspace_root: workspace.to_path_buf(),
        tool_cache_key,
        timeout_secs: server_timeout_secs(server, config),
        next_request_id: 2,
        child,
        stdin,
        reader: BufReader::new(stdout),
        stderr_task,
        stdout_lines,
        stderr_lines,
    };
    if let Err(error) = session.initialize().await {
        let decorated = session.decorate_error(error);
        session.shutdown().await;
        record_spawn_failure(server_name);
        return Err(decorated);
    }
    clear_spawn_failure(server_name);
    Ok(session)
}

async fn get_or_create_server_session(
    server_name: &str,
    config: &Config,
    workspace: &Path,
) -> Result<(String, Arc<AsyncMutex<McpServerSession>>), String> {
    let server = config
        .mcp_servers
        .get(server_name)
        .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
    let key = cache_key(server_name, server, workspace, config)?;
    let now = Instant::now();

    reap_idle_server_sessions(now).await?;

    if let Some(existing) = {
        let mut cache = session_cache()
            .lock()
            .map_err(|_| "MCP session cache lock poisoned".to_string())?;
        match cache.get_mut(&key) {
            Some(entry) => {
                entry.last_used_at = now;
                Some(entry.session.clone())
            }
            None => None,
        }
    } {
        return Ok((key, existing));
    }

    let created = Arc::new(AsyncMutex::new(
        spawn_server_session(server_name, config, workspace).await?,
    ));
    let existing = {
        let mut cache = session_cache()
            .lock()
            .map_err(|_| "MCP session cache lock poisoned".to_string())?;
        if let Some(existing) = cache.get_mut(&key) {
            existing.last_used_at = now;
            Some(existing.session.clone())
        } else {
            cache.insert(
                key.clone(),
                CachedMcpSession {
                    session: created.clone(),
                    last_used_at: now,
                },
            );
            None
        }
    };

    if let Some(existing) = existing {
        let mut created_guard = created.lock().await;
        created_guard.shutdown().await;
        Ok((key, existing))
    } else {
        Ok((key, created))
    }
}

async fn reap_idle_server_sessions(now: Instant) -> Result<(), String> {
    let stale = {
        let mut cache = session_cache()
            .lock()
            .map_err(|_| "MCP session cache lock poisoned".to_string())?;
        let stale_keys: Vec<String> = cache
            .iter()
            .filter_map(|(cache_key, entry)| {
                if now.duration_since(entry.last_used_at) >= session_idle_ttl() {
                    Some(cache_key.clone())
                } else {
                    None
                }
            })
            .collect();
        let mut stale = Vec::with_capacity(stale_keys.len());
        for cache_key in stale_keys {
            let removed_entry = cache.remove(&cache_key);
            if let Some(entry) = removed_entry {
                stale.push(entry.session);
            }
        }
        stale
    };

    for session in stale {
        let mut guard = session.lock().await;
        guard.shutdown().await;
    }

    Ok(())
}

fn remove_cached_server_session(cache_key: &str, session: &Arc<AsyncMutex<McpServerSession>>) {
    if let Ok(mut cache) = session_cache().lock()
        && let Some(existing) = cache.get(cache_key)
        && Arc::ptr_eq(&existing.session, session)
    {
        cache.remove(cache_key);
    }
}

async fn call_server_once(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let server = config
        .mcp_servers
        .get(server_name)
        .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
    if !server.enabled {
        return Err(format!("MCP server '{server_name}' is disabled"));
    }

    let mut session = spawn_server_session(server_name, config, workspace).await?;
    let result = match session.request(method, params).await {
        Ok(result) => Ok(result),
        Err(error) => Err(session.decorate_error(error)),
    };
    session.shutdown().await;
    result
}

async fn call_server(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let server = config
        .mcp_servers
        .get(server_name)
        .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
    if !server.enabled {
        return Err(format!("MCP server '{server_name}' is disabled"));
    }

    let (cache_key, mut session) =
        get_or_create_server_session(server_name, config, workspace).await?;

    for attempt in 0..2 {
        let request_result = {
            let mut guard = session.lock().await;
            let req_result = guard.request(method, params.clone()).await;
            match req_result {
                Ok(result) => return Ok(result),
                Err(error) => {
                    let decorated = guard.decorate_error(error);
                    let should_reset = should_reset_mcp_session(&decorated);
                    if should_reset {
                        guard.shutdown().await;
                    }
                    (decorated, should_reset)
                }
            }
        };

        let (error, should_reset) = request_result;
        if !should_reset || attempt == 1 {
            if should_reset {
                remove_cached_server_session(&cache_key, &session);
                // Ensure the orphaned session is fully cleaned up (stderr_task, child).
                let mut guard = session.lock().await;
                guard.shutdown().await;
            }
            return Err(error);
        }

        remove_cached_server_session(&cache_key, &session);
        session = get_or_create_server_session(server_name, config, workspace)
            .await?
            .1;
    }

    Err(format!("MCP call failed for '{server_name}'"))
}

async fn write_message<W>(stdin: &mut W, message: &Value) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    let mut body = serde_json::to_vec(message).map_err(|error| error.to_string())?;
    body.push(b'\n');
    stdin
        .write_all(&body)
        .await
        .map_err(|error| error.to_string())?;
    stdin.flush().await.map_err(|error| error.to_string())
}

async fn read_response<R, W>(
    reader: &mut BufReader<R>,
    stdin: &mut W,
    expected_id: u64,
    stdout_lines: &Arc<Mutex<Vec<String>>>,
    server_name: &str,
    workspace_root: &Path,
    tool_cache_key: &str,
) -> Result<Value, String>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let message = read_message(reader, stdout_lines).await?;
        if message.get("id").and_then(Value::as_u64) == Some(expected_id) {
            return Ok(message);
        }
        handle_server_message(
            stdin,
            &message,
            stdout_lines,
            server_name,
            workspace_root,
            tool_cache_key,
        )
        .await?;
    }
}

async fn handle_server_message<W>(
    stdin: &mut W,
    message: &Value,
    stdout_lines: &Arc<Mutex<Vec<String>>>,
    server_name: &str,
    workspace_root: &Path,
    tool_cache_key: &str,
) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        record_diagnostic_line(
            stdout_lines,
            &serde_json::to_string(message).unwrap_or_else(|_| message.to_string()),
        );

        if method == "notifications/tools/list_changed" {
            remove_cached_tool_descriptors(tool_cache_key);
        }

        if let Some(id) = message.get("id") {
            let response = match method {
                "ping" => json!({
                    "jsonrpc": "2.0",
                    "id": id.clone(),
                    "result": {}
                }),
                "roots/list" => json!({
                    "jsonrpc": "2.0",
                    "id": id.clone(),
                    "result": workspace_roots_result(server_name, workspace_root)
                }),
                _ => json!({
                    "jsonrpc": "2.0",
                    "id": id.clone(),
                    "error": {
                        "code": -32601,
                        "message": format!("Method not supported: {method}")
                    }
                }),
            };
            write_message(stdin, &response).await?;
        }
    }

    Ok(())
}

async fn read_message<R>(
    reader: &mut BufReader<R>,
    stdout_lines: &Arc<Mutex<Vec<String>>>,
) -> Result<Value, String>
where
    R: AsyncRead + Unpin,
{
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .await
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("MCP server closed stdout".into());
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }

        if line.starts_with('{') || line.starts_with('[') {
            match serde_json::from_str::<Value>(line) {
                Ok(message) => return Ok(message),
                Err(_) => record_diagnostic_line(stdout_lines, line),
            }
            continue;
        }

        if let Some(value) = line.strip_prefix("Content-Length:") {
            let content_length = value
                .trim()
                .parse::<usize>()
                .map_err(|error| format!("invalid Content-Length: {error}"))?;
            return read_content_length_message(reader, content_length).await;
        }

        record_diagnostic_line(stdout_lines, line);
    }
}

async fn read_content_length_message<R>(
    reader: &mut BufReader<R>,
    content_length: usize,
) -> Result<Value, String>
where
    R: AsyncRead + Unpin,
{
    loop {
        let mut header_line = String::new();
        let read = reader
            .read_line(&mut header_line)
            .await
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err("MCP server closed stdout while reading headers".into());
        }
        if header_line.trim_end_matches(['\r', '\n']).is_empty() {
            break;
        }
    }

    let mut body = vec![0_u8; content_length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&body).map_err(|error| format!("invalid MCP JSON: {error}"))
}

#[cfg(test)]
async fn write_message_for_test(message: &Value) -> Result<Vec<u8>, String> {
    let (mut writer, mut reader) = tokio::io::duplex(1024);
    let payload = message.clone();
    let writer_task = tokio::spawn(async move {
        write_message(&mut writer, &payload)
            .await
            .expect("write should succeed");
    });

    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    writer_task.await.map_err(|error| error.to_string())?;
    Ok(bytes)
}

#[cfg(test)]
#[path = "../tests/mcp_tests.rs"]
mod mcp_tests;
