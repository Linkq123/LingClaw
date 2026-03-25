use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{ChildStderr, Command},
};

use crate::{config::JsonMcpServerConfig, resolve_path_checked, Config, VERSION};

use super::ToolOutcome;

const MCP_NAME_PREFIX: &str = "mcp__";
const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const MCP_DIAGNOSTIC_LINE_LIMIT: usize = 6;
const MCP_DIAGNOSTIC_CHAR_LIMIT: usize = 400;
static MCP_TOOL_CACHE: OnceLock<Mutex<HashMap<String, Vec<McpToolDescriptor>>>> = OnceLock::new();

#[derive(Clone, Debug)]
struct McpToolDescriptor {
    server_name: String,
    raw_name: String,
    exposed_name: String,
    description: String,
    input_schema: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct McpServerLoadReport {
    pub(crate) server_name: String,
    pub(crate) tool_names: Vec<String>,
    pub(crate) error: Option<String>,
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

    let mut reports = Vec::with_capacity(server_names.len());
    for server_name in server_names {
        match list_server_tools(server_name, config, workspace).await {
            Ok(tools) => reports.push(McpServerLoadReport {
                server_name: server_name.to_string(),
                tool_names: tools.into_iter().map(|tool| tool.exposed_name).collect(),
                error: None,
            }),
            Err(error) => reports.push(McpServerLoadReport {
                server_name: server_name.to_string(),
                tool_names: Vec::new(),
                error: Some(error),
            }),
        }
    }

    reports
}

fn tool_cache() -> &'static Mutex<HashMap<String, Vec<McpToolDescriptor>>> {
    MCP_TOOL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
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
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    parts.push(text.to_string());
                    continue;
                }
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
    for server_name in server_names {
        match list_server_tools(server_name, config, workspace).await {
            Ok(mut server_tools) => tools.append(&mut server_tools),
            Err(error) => {
                eprintln!("Warning: MCP server '{server_name}' unavailable: {error}");
            }
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

    let cached = {
        let cache = tool_cache()
            .lock()
            .map_err(|_| "MCP tool cache lock poisoned".to_string())?;
        cache.get(&key).cloned()
    };
    if let Some(cached) = cached {
        return Ok(cached);
    }

    let response = call_server(server_name, config, workspace, "tools/list", json!({})).await?;
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

    {
        let mut cache = tool_cache()
            .lock()
            .map_err(|_| "MCP tool cache lock poisoned".to_string())?;
        cache.insert(key, descriptors.clone());
    }

    Ok(descriptors)
}

fn server_timeout_secs(server: &JsonMcpServerConfig, config: &Config) -> u64 {
    server.timeout_secs.unwrap_or(config.tool_timeout.as_secs())
}

fn resolve_server_cwd(server: &JsonMcpServerConfig, workspace: &Path) -> Result<PathBuf, String> {
    match server.cwd.as_deref() {
        Some(cwd) if !cwd.is_empty() => resolve_path_checked(cwd, workspace)
            .map_err(|message| format!("MCP server cwd '{}' is invalid: {message}", cwd)),
        _ => Ok(workspace.to_path_buf()),
    }
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

    let server_cwd = resolve_server_cwd(server, workspace)?;

    let mut command = Command::new(&server.command);
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

    let timeout_secs = server_timeout_secs(server, config);

    let stdout_lines = Arc::new(Mutex::new(Vec::new()));
    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    let timeout_stdout = stdout_lines.clone();
    let timeout_stderr = stderr_lines.clone();

    async move {
        let mut child = command
            .spawn()
            .map_err(|error| format!("failed to spawn '{}': {error}", server.command))?;
        let mut stdin = child
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
        let mut reader = BufReader::new(stdout);
        let stderr_task = tokio::spawn(collect_stderr_lines(stderr, stderr_lines.clone()));

        write_message(
            &mut stdin,
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
            Duration::from_secs(timeout_secs),
            read_response(&mut reader, 1, &stdout_lines),
        )
        .await
        .map_err(|_| {
            format_mcp_timeout_error(
                "initialize",
                timeout_secs,
                &snapshot_diagnostic_lines(&stdout_lines),
                &snapshot_diagnostic_lines(&stderr_lines),
            )
        })??;
        if let Some(error) = initialize.get("error") {
            return Err(format!(
                "initialize failed: {}",
                serde_json::to_string(error).unwrap_or_else(|_| error.to_string())
            ));
        }

        write_message(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
        )
        .await?;

        write_message(
            &mut stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": method,
                "params": params,
            }),
        )
        .await?;
        let response = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            read_response(&mut reader, 2, &stdout_lines),
        )
        .await
        .map_err(|_| {
            format_mcp_timeout_error(
                method,
                timeout_secs,
                &snapshot_diagnostic_lines(&stdout_lines),
                &snapshot_diagnostic_lines(&stderr_lines),
            )
        })??;

        let _ = stdin.shutdown().await;
        let _ = child.start_kill();
        let _ = child.wait().await;
        let _ = stderr_task.await;

        if let Some(error) = response.get("error") {
            return Err(serde_json::to_string(error).unwrap_or_else(|_| error.to_string()));
        }

        response
            .get("result")
            .cloned()
            .ok_or_else(|| format!("server '{server_name}' response missing result"))
    }
    .await
    .map_err(|error| {
        if error.contains("timed out after") || error.contains("initialize failed") {
            return error;
        }
        format!(
            "{error}{}",
            format_mcp_diagnostics(
                &snapshot_diagnostic_lines(&timeout_stdout),
                &snapshot_diagnostic_lines(&timeout_stderr),
            )
        )
    })
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

async fn read_response<R>(
    reader: &mut BufReader<R>,
    expected_id: u64,
    stdout_lines: &Arc<Mutex<Vec<String>>>,
) -> Result<Value, String>
where
    R: AsyncRead + Unpin,
{
    loop {
        let message = read_message(reader, stdout_lines).await?;
        if message.get("id").and_then(Value::as_u64) == Some(expected_id) {
            return Ok(message);
        }
    }
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
