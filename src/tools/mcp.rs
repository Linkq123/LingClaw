use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::{StreamExt, future::join_all};
use reqwest::StatusCode as HttpStatusCode;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::Mutex as AsyncMutex,
    task::JoinHandle,
};

use crate::{Config, VERSION, config::JsonMcpServerConfig, config_dir_path, resolve_path_checked};

use super::ToolOutcome;

const MCP_NAME_PREFIX: &str = "mcp__";
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_DIAGNOSTIC_LINE_LIMIT: usize = 6;
const MCP_DIAGNOSTIC_CHAR_LIMIT: usize = 400;
const MCP_TOOL_CACHE_TTL_SECS: u64 = 30;
const MCP_SESSION_IDLE_TTL_SECS: u64 = 300;
const MCP_SPAWN_FAILURE_COOLDOWN_SECS: u64 = 15;
#[cfg(test)]
const MCP_DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;
const MCP_MAX_PAGINATION_PAGES: usize = 100;
const MCP_SESSION_POLICY_FILE: &str = ".lingclaw-mcp-policy.json";
const MCP_AUTH_FILE: &str = "mcp-auth.json";
static MCP_TOOL_CACHE: OnceLock<Mutex<HashMap<String, CachedToolDescriptors>>> = OnceLock::new();
static MCP_RESOURCE_CACHE: OnceLock<Mutex<HashMap<String, CachedResourceDescriptors>>> =
    OnceLock::new();
static MCP_PROMPT_CACHE: OnceLock<Mutex<HashMap<String, CachedPromptDescriptors>>> =
    OnceLock::new();
static MCP_SESSION_CACHE: OnceLock<Mutex<HashMap<String, CachedMcpSession>>> = OnceLock::new();
static MCP_HTTP_SESSION_CACHE: OnceLock<Mutex<HashMap<String, CachedHttpMcpSession>>> =
    OnceLock::new();
static MCP_HTTP_STREAM_TASKS: OnceLock<Mutex<HashMap<String, HttpStreamTaskEntry>>> =
    OnceLock::new();
static MCP_HTTP_LAST_EVENT_IDS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static MCP_HTTP_INITIALIZATION_LOCKS: OnceLock<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>> =
    OnceLock::new();
static MCP_SPAWN_FAILURES: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
static MCP_NEXT_HTTP_STREAM_TASK_ID: AtomicU64 = AtomicU64::new(1);
static MCP_NEXT_HTTP_REQUEST_ID: AtomicU64 = AtomicU64::new(2);
#[cfg(test)]
static MCP_AUTH_FILE_OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
#[cfg(test)]
static MCP_TEST_GUARD: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

#[cfg(test)]
pub(crate) async fn acquire_mcp_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    MCP_TEST_GUARD
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .lock()
        .await
}

#[derive(Clone, Debug)]
pub(crate) struct McpToolDescriptor {
    pub(crate) server_name: String,
    pub(crate) raw_name: String,
    pub(crate) exposed_name: String,
    pub(crate) description: String,
    pub(crate) input_schema: Value,
}

#[derive(Clone, Debug)]
pub(crate) struct McpResourceDescriptor {
    pub(crate) server_name: String,
    pub(crate) uri: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) mime_type: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct McpPromptDescriptor {
    pub(crate) server_name: String,
    pub(crate) raw_name: String,
    pub(crate) description: String,
    pub(crate) arguments: Value,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpClientCapabilityPolicy {
    #[serde(default)]
    pub(crate) roots: bool,
    #[serde(default)]
    pub(crate) sampling: bool,
    #[serde(default)]
    pub(crate) elicitation: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpSessionPolicy {
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub(crate) enabled_servers: HashSet<String>,
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub(crate) enabled_tools: HashSet<String>,
    #[serde(default)]
    pub(crate) confirm_mutating_tools: bool,
    #[serde(default)]
    pub(crate) client_capabilities: McpClientCapabilityPolicy,
}

impl McpSessionPolicy {
    pub(crate) fn allows_server(&self, server_name: &str) -> bool {
        self.enabled_servers.contains(server_name)
    }

    pub(crate) fn allows_tool(&self, descriptor: &McpToolDescriptor) -> bool {
        self.allows_server(&descriptor.server_name)
            && self.enabled_tools.contains(&descriptor.exposed_name)
    }
}

#[derive(Clone, Debug)]
struct CachedToolDescriptors {
    descriptors: Vec<McpToolDescriptor>,
    loaded_at: Instant,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct CachedResourceDescriptors {
    descriptors: Vec<McpResourceDescriptor>,
    loaded_at: Instant,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct CachedPromptDescriptors {
    descriptors: Vec<McpPromptDescriptor>,
    loaded_at: Instant,
}

#[derive(Clone, Debug)]
pub(crate) struct McpServerLoadReport {
    pub(crate) server_name: String,
    pub(crate) transport: String,
    pub(crate) tool_names: Vec<String>,
    pub(crate) resource_count: usize,
    pub(crate) prompt_count: usize,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct McpCatalogSnapshot {
    pub(crate) tools: Vec<McpToolDescriptor>,
    pub(crate) resources: Vec<McpResourceDescriptor>,
    pub(crate) prompts: Vec<McpPromptDescriptor>,
    pub(crate) reports: Vec<McpServerLoadReport>,
}

#[derive(Debug, Default)]
struct McpServerCatalogLoad {
    tools: Vec<McpToolDescriptor>,
    resources: Vec<McpResourceDescriptor>,
    prompts: Vec<McpPromptDescriptor>,
    tools_loaded: bool,
    resources_loaded: bool,
    prompts_loaded: bool,
    error: Option<String>,
}

struct CachedMcpSession {
    session: Arc<AsyncMutex<McpServerSession>>,
    last_used_at: Instant,
}

struct CachedHttpMcpSession {
    session_id: Option<String>,
    last_used_at: Instant,
}

struct HttpStreamTaskEntry {
    task_id: u64,
    handle: JoinHandle<()>,
}

enum TemporaryMcpSession {
    Http {
        server_name: String,
        server: JsonMcpServerConfig,
        cache_key: String,
        session_id: Option<String>,
        timeout_secs: u64,
    },
    Stdio(McpServerSession),
}

struct McpServerSession {
    server_name: String,
    workspace_root: PathBuf,
    tool_cache_key: String,
    client_capabilities: McpClientCapabilityPolicy,
    timeout_secs: u64,
    next_request_id: u64,
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    stderr_task: Option<JoinHandle<()>>,
    stdout_lines: Arc<Mutex<Vec<String>>>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
}

pub(crate) fn is_mcp_tool_name(name: &str) -> bool {
    name.starts_with(MCP_NAME_PREFIX)
}

/// Conservative read-only classification for MCP tools.
/// Unknown tools default to mutating. A tool is considered read-only only when
/// it avoids mutation keywords and explicitly matches a read-only keyword.
pub(crate) fn is_read_only_tool_descriptor(descriptor: &McpToolDescriptor) -> bool {
    const MUTATION_WORDS: &[&str] = &[
        "write",
        "clone",
        "create",
        "update",
        "delete",
        "remove",
        "modify",
        "set",
        "put",
        "post",
        "patch",
        "insert",
        "add",
        "edit",
        "append",
        "replace",
        "rename",
        "move",
        "copy",
        "checkout",
        "switch",
        "execute",
        "run",
        "exec",
        "deploy",
        "install",
        "uninstall",
        "send",
        "publish",
        "push",
        "commit",
        "approve",
        "merge",
        "close",
        "reopen",
        "assign",
        "drop",
        "truncate",
        "grant",
        "revoke",
        "enable",
        "disable",
        "start",
        "stop",
        "restart",
        "kill",
        "terminate",
        "upload",
        "submit",
        "apply",
        "reset",
        "purge",
        "destroy",
        "dismiss",
        "invite",
        "ban",
        "block",
        "archive",
    ];
    const READ_ONLY_WORDS: &[&str] = &[
        "get", "read", "list", "search", "find", "fetch", "lookup", "describe", "show", "inspect",
        "retrieve", "view", "stat", "status", "count",
    ];

    let name = descriptor.raw_name.to_lowercase();
    let desc = descriptor.description.to_lowercase();
    let name_words = name.split(|c: char| !c.is_alphanumeric());
    let desc_words = desc.split(|c: char| !c.is_alphanumeric());
    let mut saw_read_only_keyword = false;

    for word in name_words.chain(desc_words) {
        if word.is_empty() {
            continue;
        }
        if MUTATION_WORDS.contains(&word) {
            return false;
        }
        if READ_ONLY_WORDS.contains(&word) {
            saw_read_only_keyword = true;
        }
    }

    saw_read_only_keyword
}

/// Cached-only lookup for MCP parallel classification.
/// Cache misses are treated as mutating so scheduling never has to spawn or
/// probe an MCP server before tool execution begins.
pub(crate) fn is_read_only_tool_name(name: &str, config: &Config, workspace: &Path) -> bool {
    if !is_mcp_tool_name(name) {
        return false;
    }

    cached_list_tools(config, workspace)
        .into_iter()
        .find(|descriptor| descriptor.exposed_name == name)
        .is_some_and(|descriptor| is_read_only_tool_descriptor(&descriptor))
}

pub(crate) fn runtime_tool_note(config: &Config, workspace: &Path) -> Option<String> {
    let policy = load_session_policy(workspace);
    if policy.enabled_tools.is_empty() {
        return None;
    }
    let mut names: Vec<&str> = config
        .mcp_servers
        .iter()
        .filter(|(name, server)| {
            server.enabled
                && policy.enabled_servers.contains(*name)
                && policy
                    .enabled_tools
                    .iter()
                    .any(|tool| exposed_tool_matches_server(tool, name))
        })
        .map(|(name, _)| name.as_str())
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort_unstable();
    Some(format!(
        "MCP tools enabled for this session are available from servers: {}. MCP tool names are prefixed with 'mcp__'.",
        names.join(", ")
    ))
}

fn session_policy_path(workspace: &Path) -> PathBuf {
    workspace.join(MCP_SESSION_POLICY_FILE)
}

pub(crate) fn load_session_policy(workspace: &Path) -> McpSessionPolicy {
    let path = session_policy_path(workspace);
    let Ok(text) = fs::read_to_string(&path) else {
        return McpSessionPolicy::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub(crate) fn save_session_policy(
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Result<(), String> {
    fs::create_dir_all(workspace)
        .map_err(|error| format!("failed to create MCP policy directory: {error}"))?;
    let path = session_policy_path(workspace);
    let text = serde_json::to_string_pretty(policy)
        .map_err(|error| format!("failed to encode MCP session policy: {error}"))?;
    fs::write(&path, text).map_err(|error| format!("failed to write MCP session policy: {error}"))
}

fn auth_file_path() -> PathBuf {
    #[cfg(test)]
    if let Ok(guard) = MCP_AUTH_FILE_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        && let Some(path) = guard.clone()
    {
        return path;
    }

    config_dir_path()
        .unwrap_or_else(|| PathBuf::from(".lingclaw"))
        .join(MCP_AUTH_FILE)
}

#[cfg(test)]
pub(crate) fn set_auth_file_path_for_test(path: PathBuf) {
    if let Ok(mut guard) = MCP_AUTH_FILE_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
    {
        *guard = Some(path);
    }
}

#[cfg(test)]
pub(crate) fn reset_auth_file_path_for_test() {
    if let Ok(mut guard) = MCP_AUTH_FILE_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
    {
        *guard = None;
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpAuthState {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub(crate) servers: HashMap<String, McpServerAuthState>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpServerAuthState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) resource: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) authorization_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pending: Option<McpPendingOAuthState>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpPendingOAuthState {
    pub(crate) state: String,
    pub(crate) code_verifier: String,
    pub(crate) redirect_uri: String,
    pub(crate) token_endpoint: String,
    pub(crate) client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) resource: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct McpOAuthStartResult {
    pub(crate) server: String,
    pub(crate) authorization_url: String,
    pub(crate) redirect_uri: String,
    pub(crate) client_id: String,
    pub(crate) scopes: Vec<String>,
}

pub(crate) fn load_auth_state() -> McpAuthState {
    let path = auth_file_path();
    let Ok(text) = fs::read_to_string(&path) else {
        return McpAuthState::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub(crate) fn save_auth_state(state: &McpAuthState) -> Result<(), String> {
    let path = auth_file_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create MCP auth directory: {error}"))?;
    }
    let text = serde_json::to_string_pretty(state)
        .map_err(|error| format!("failed to encode MCP auth state: {error}"))?;
    #[cfg(unix)]
    {
        use std::{io::Write, os::unix::fs::OpenOptionsExt, os::unix::fs::PermissionsExt};
        if path.exists() {
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| format!("failed to write MCP auth state: {error}"))?;
        file.write_all(text.as_bytes())
            .map_err(|error| format!("failed to write MCP auth state: {error}"))?;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    {
        fs::write(&path, text)
            .map_err(|error| format!("failed to write MCP auth state: {error}"))?;
    }
    Ok(())
}

fn random_urlsafe(bytes_len: usize) -> Result<String, String> {
    let mut bytes = vec![0_u8; bytes_len];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| format!("failed to generate random data: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn reqwest_client_with_timeout(timeout_secs: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(timeout_secs.max(1)))
        .build()
        .map_err(|error| format!("failed to build HTTP client: {error}"))
}

async fn send_http_request_with_timeout(
    request: reqwest::RequestBuilder,
    timeout_secs: u64,
    context: &str,
) -> Result<reqwest::Response, String> {
    let timeout_secs = timeout_secs.max(1);
    match tokio::time::timeout(Duration::from_secs(timeout_secs), request.send()).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(error)) => Err(format!("{context} failed: {error}")),
        Err(_) => Err(format!("{context} timed out after {timeout_secs}s")),
    }
}

async fn response_text_with_timeout(
    response: reqwest::Response,
    timeout_secs: u64,
    context: &str,
) -> Result<String, String> {
    let timeout_secs = timeout_secs.max(1);
    match tokio::time::timeout(Duration::from_secs(timeout_secs), response.text()).await {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(error)) => Err(format!("{context}: {error}")),
        Err(_) => Err(format!("{context} timed out after {timeout_secs}s")),
    }
}

async fn response_json_with_timeout<T>(
    response: reqwest::Response,
    timeout_secs: u64,
    context: &str,
) -> Result<T, String>
where
    T: DeserializeOwned,
{
    let timeout_secs = timeout_secs.max(1);
    match tokio::time::timeout(Duration::from_secs(timeout_secs), response.json::<T>()).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(format!("{context} is not JSON: {error}")),
        Err(_) => Err(format!("{context} timed out after {timeout_secs}s")),
    }
}

fn effective_client_capabilities(policy: &McpClientCapabilityPolicy) -> McpClientCapabilityPolicy {
    McpClientCapabilityPolicy {
        // Roots is implemented locally by returning the current workspace root.
        roots: policy.roots,
        // Sampling and elicitation require an interactive client bridge; do not
        // advertise them until the runtime can fulfill those server requests.
        sampling: false,
        elicitation: false,
    }
}

fn client_capabilities_for_server(
    server_name: &str,
    workspace: &Path,
) -> McpClientCapabilityPolicy {
    let policy = load_session_policy(workspace);
    if policy.allows_server(server_name) {
        effective_client_capabilities(&policy.client_capabilities)
    } else {
        McpClientCapabilityPolicy::default()
    }
}

fn initialize_capabilities(policy: &McpClientCapabilityPolicy) -> Value {
    let mut capabilities = serde_json::Map::new();
    let policy = effective_client_capabilities(policy);
    if policy.roots {
        capabilities.insert("roots".to_string(), json!({ "listChanged": false }));
    }
    if policy.sampling {
        capabilities.insert("sampling".to_string(), json!({}));
    }
    if policy.elicitation {
        capabilities.insert("elicitation".to_string(), json!({}));
    }
    Value::Object(capabilities)
}

fn server_base_url(server: &JsonMcpServerConfig) -> Result<reqwest::Url, String> {
    let url = server
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "streamable-http MCP server is missing url".to_string())?;
    reqwest::Url::parse(url).map_err(|error| format!("invalid streamable-http MCP url: {error}"))
}

fn origin_well_known(url: &reqwest::Url, suffix: &str) -> Result<reqwest::Url, String> {
    let mut base = url.clone();
    base.set_path(suffix);
    base.set_query(None);
    base.set_fragment(None);
    Ok(base)
}

fn path_well_known(url: &reqwest::Url, suffix: &str) -> Result<reqwest::Url, String> {
    let mut next = url.clone();
    let path = url.path().trim_start_matches('/');
    let suffix = suffix.trim_start_matches('/');
    let combined = if path.is_empty() {
        format!("/{suffix}")
    } else {
        format!("/{suffix}/{path}")
    };
    next.set_path(&combined);
    next.set_query(None);
    next.set_fragment(None);
    Ok(next)
}

fn append_well_known(url: &reqwest::Url, suffix: &str) -> Result<reqwest::Url, String> {
    let mut next = url.clone();
    let path = url.path().trim_end_matches('/');
    let suffix = suffix.trim_start_matches('/');
    let combined = if path.is_empty() {
        format!("/{suffix}")
    } else {
        format!("{path}/{suffix}")
    };
    next.set_path(&combined);
    next.set_query(None);
    next.set_fragment(None);
    Ok(next)
}

fn parse_www_authenticate_metadata(value: &str) -> Option<String> {
    let marker = "resource_metadata";
    let start = value.find(marker)? + marker.len();
    let rest = value[start..].trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    if let Some(rest) = rest.strip_prefix('"') {
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    } else {
        let end = rest.find([',', ' ']).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

async fn fetch_json_url(
    client: &reqwest::Client,
    url: reqwest::Url,
    timeout_secs: u64,
) -> Result<Value, String> {
    let request = client.get(url.clone()).header("accept", "application/json");
    let response =
        send_http_request_with_timeout(request, timeout_secs, &format!("metadata request {url}"))
            .await?;
    if !response.status().is_success() {
        return Err(format!(
            "metadata request {url} failed with {}",
            response.status()
        ));
    }
    response_json_with_timeout(response, timeout_secs, &format!("metadata response {url}")).await
}

async fn discover_resource_metadata(
    client: &reqwest::Client,
    server: &JsonMcpServerConfig,
    timeout_secs: u64,
) -> Result<Value, String> {
    let server_url = server_base_url(server)?;

    let mut init = client
        .post(server_url.clone())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "LingClaw", "version": VERSION}
            }
        }));
    for (key, value) in &server.headers {
        init = init.header(key, resolve_env_placeholder(value));
    }
    if let Ok(response) =
        send_http_request_with_timeout(init, timeout_secs, "OAuth protected resource probe").await
        && response.status() == HttpStatusCode::UNAUTHORIZED
        && let Some(metadata_url) = response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok())
            .and_then(parse_www_authenticate_metadata)
        && let Ok(url) = reqwest::Url::parse(&metadata_url)
        && let Ok(metadata) = fetch_json_url(client, url, timeout_secs).await
    {
        return Ok(metadata);
    }

    let mut candidates = Vec::new();
    candidates.push(path_well_known(
        &server_url,
        "/.well-known/oauth-protected-resource",
    )?);
    candidates.push(origin_well_known(
        &server_url,
        "/.well-known/oauth-protected-resource",
    )?);
    for candidate in candidates {
        if let Ok(metadata) = fetch_json_url(client, candidate, timeout_secs).await {
            return Ok(metadata);
        }
    }
    Err("failed to discover OAuth protected resource metadata".to_string())
}

async fn discover_authorization_metadata(
    client: &reqwest::Client,
    resource_metadata: &Value,
    timeout_secs: u64,
) -> Result<Value, String> {
    let issuer = resource_metadata
        .get("authorization_servers")
        .and_then(Value::as_array)
        .and_then(|servers| servers.first())
        .and_then(Value::as_str)
        .or_else(|| {
            resource_metadata
                .get("authorization_server")
                .and_then(Value::as_str)
        })
        .ok_or_else(|| {
            "protected resource metadata did not declare authorization_servers".to_string()
        })?;
    let issuer_url = reqwest::Url::parse(issuer)
        .map_err(|error| format!("invalid authorization server URL: {error}"))?;

    let candidates = [
        path_well_known(&issuer_url, "/.well-known/oauth-authorization-server")?,
        origin_well_known(&issuer_url, "/.well-known/oauth-authorization-server")?,
        append_well_known(&issuer_url, "/.well-known/openid-configuration")?,
        origin_well_known(&issuer_url, "/.well-known/openid-configuration")?,
    ];
    for candidate in candidates {
        if let Ok(metadata) = fetch_json_url(client, candidate, timeout_secs).await {
            return Ok(metadata);
        }
    }
    Err("failed to discover OAuth authorization server metadata".to_string())
}

async fn register_oauth_client(
    client: &reqwest::Client,
    registration_endpoint: &str,
    redirect_uri: &str,
    timeout_secs: u64,
) -> Result<(String, Option<String>), String> {
    let request = client
        .post(registration_endpoint)
        .header("content-type", "application/json")
        .json(&json!({
            "client_name": "LingClaw",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        }));
    let response =
        send_http_request_with_timeout(request, timeout_secs, "OAuth dynamic client registration")
            .await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response_text_with_timeout(
            response,
            timeout_secs,
            "failed to read OAuth dynamic client registration error response",
        )
        .await
        .unwrap_or_default();
        return Err(format!(
            "OAuth dynamic client registration failed with {status}: {text}"
        ));
    }
    let payload =
        response_json_with_timeout::<Value>(response, timeout_secs, "OAuth registration response")
            .await?;
    let client_id = payload
        .get("client_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "OAuth registration response missing client_id".to_string())?;
    let client_secret = payload
        .get("client_secret")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok((client_id, client_secret))
}

pub(crate) async fn start_oauth_authorization(
    server_name: &str,
    server: &JsonMcpServerConfig,
    local_port: u16,
    default_timeout_secs: u64,
) -> Result<McpOAuthStartResult, String> {
    let timeout_secs = server.timeout_secs.unwrap_or(default_timeout_secs).max(1);
    let client = reqwest_client_with_timeout(timeout_secs)?;
    let resource_metadata = discover_resource_metadata(&client, server, timeout_secs).await?;
    let auth_metadata =
        discover_authorization_metadata(&client, &resource_metadata, timeout_secs).await?;
    let authorization_endpoint = auth_metadata
        .get("authorization_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "authorization server metadata missing authorization_endpoint".to_string()
        })?;
    let token_endpoint = auth_metadata
        .get("token_endpoint")
        .and_then(Value::as_str)
        .ok_or_else(|| "authorization server metadata missing token_endpoint".to_string())?;
    let mut redirect_url = reqwest::Url::parse(&format!(
        "http://127.0.0.1:{local_port}/api/mcp/auth/callback"
    ))
    .map_err(|error| format!("invalid OAuth callback URL: {error}"))?;
    redirect_url
        .query_pairs_mut()
        .append_pair("server", server_name);
    let redirect_uri = redirect_url.to_string();

    let configured_client_id = server
        .auth
        .as_ref()
        .and_then(|auth| auth.client_id.as_deref())
        .map(resolve_env_placeholder)
        .filter(|value| !value.trim().is_empty());
    let configured_client_secret = server
        .auth
        .as_ref()
        .and_then(|auth| auth.client_secret.as_deref())
        .map(resolve_env_placeholder)
        .filter(|value| !value.trim().is_empty());
    let (client_id, client_secret) = if let Some(client_id) = configured_client_id {
        (client_id, configured_client_secret)
    } else if let Some(registration_endpoint) = auth_metadata
        .get("registration_endpoint")
        .and_then(Value::as_str)
    {
        register_oauth_client(&client, registration_endpoint, &redirect_uri, timeout_secs).await?
    } else {
        return Err(
            "OAuth server does not advertise dynamic registration; configure auth.clientId"
                .to_string(),
        );
    };

    let scopes = server
        .auth
        .as_ref()
        .map(|auth| auth.scopes.clone())
        .unwrap_or_default();
    let resource = resource_metadata
        .get("resource")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| server.url.clone());
    let state = random_urlsafe(18)?;
    let code_verifier = random_urlsafe(32)?;
    let code_challenge = pkce_challenge(&code_verifier);
    let mut authorization_url = reqwest::Url::parse(authorization_endpoint)
        .map_err(|error| format!("invalid authorization_endpoint: {error}"))?;
    {
        let mut query = authorization_url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", &client_id);
        query.append_pair("redirect_uri", &redirect_uri);
        query.append_pair("state", &state);
        query.append_pair("code_challenge", &code_challenge);
        query.append_pair("code_challenge_method", "S256");
        if !scopes.is_empty() {
            query.append_pair("scope", &scopes.join(" "));
        }
        if let Some(resource) = resource.as_deref() {
            query.append_pair("resource", resource);
        }
    }

    let mut auth_state = load_auth_state();
    let mut next_auth = auth_state
        .servers
        .get(server_name)
        .cloned()
        .unwrap_or_default();
    let previous_token_has_compatible_binding = match next_auth.resource.as_deref() {
        None => true,
        Some(value) => resource
            .as_deref()
            .is_some_and(|next| trim_url_slashes(value) == trim_url_slashes(next)),
    };
    let previous_token_exists =
        next_auth.access_token.is_some() || next_auth.refresh_token.is_some();
    let replacing_current_token = !previous_token_exists || !previous_token_has_compatible_binding;
    if previous_token_exists && !previous_token_has_compatible_binding {
        next_auth.access_token = None;
        next_auth.refresh_token = None;
        next_auth.expires_at = None;
        clear_cached_runtime_state_for_server(server_name);
    }
    if replacing_current_token {
        next_auth.client_id = Some(client_id.clone());
        next_auth.client_secret = client_secret.clone();
        next_auth.scopes = scopes.clone();
        next_auth.resource = resource.clone();
        next_auth.token_endpoint = Some(token_endpoint.to_string());
    }
    next_auth.authorization_endpoint = Some(authorization_endpoint.to_string());
    next_auth.pending = Some(McpPendingOAuthState {
        state,
        code_verifier,
        redirect_uri: redirect_uri.clone(),
        token_endpoint: token_endpoint.to_string(),
        client_id: client_id.clone(),
        client_secret,
        scopes: scopes.clone(),
        resource,
    });
    auth_state
        .servers
        .insert(server_name.to_string(), next_auth);
    save_auth_state(&auth_state)?;

    Ok(McpOAuthStartResult {
        server: server_name.to_string(),
        authorization_url: authorization_url.to_string(),
        redirect_uri,
        client_id,
        scopes,
    })
}

pub(crate) async fn complete_oauth_authorization(
    server_name: &str,
    code: &str,
    state: &str,
    timeout_secs: u64,
) -> Result<McpServerAuthState, String> {
    let mut auth_state = load_auth_state();
    let existing = auth_state
        .servers
        .get(server_name)
        .cloned()
        .ok_or_else(|| format!("OAuth authorization was not started for server '{server_name}'"))?;
    let pending = existing
        .pending
        .ok_or_else(|| format!("OAuth authorization is not pending for server '{server_name}'"))?;
    if pending.state != state {
        return Err("OAuth state mismatch".to_string());
    }

    let mut form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
        ("redirect_uri".to_string(), pending.redirect_uri.clone()),
        ("client_id".to_string(), pending.client_id.clone()),
        ("code_verifier".to_string(), pending.code_verifier.clone()),
    ];
    if let Some(secret) = pending.client_secret.as_deref() {
        form.push(("client_secret".to_string(), secret.to_string()));
    }
    if let Some(resource) = pending.resource.as_deref() {
        form.push(("resource".to_string(), resource.to_string()));
    }

    let timeout_secs = timeout_secs.max(1);
    let request = reqwest_client_with_timeout(timeout_secs)?
        .post(&pending.token_endpoint)
        .form(&form);
    let response =
        send_http_request_with_timeout(request, timeout_secs, "OAuth token exchange").await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response_text_with_timeout(
            response,
            timeout_secs,
            "failed to read OAuth token exchange error response",
        )
        .await
        .unwrap_or_default();
        return Err(format!("OAuth token exchange failed with {status}: {text}"));
    }
    let payload =
        response_json_with_timeout::<Value>(response, timeout_secs, "OAuth token response").await?;
    let access_token = payload
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "OAuth token response missing access_token".to_string())?;
    let refresh_token = payload
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_string);
    let expires_at = payload
        .get("expires_in")
        .and_then(Value::as_u64)
        .map(|expires_in| now_unix_secs().saturating_add(expires_in));
    let scopes = payload
        .get("scope")
        .and_then(Value::as_str)
        .map(|scope| scope.split_whitespace().map(str::to_string).collect())
        .unwrap_or_else(|| pending.scopes.clone());

    let completed = McpServerAuthState {
        access_token: Some(access_token),
        refresh_token,
        expires_at,
        scopes,
        client_id: Some(pending.client_id),
        client_secret: pending.client_secret,
        resource: pending.resource,
        token_endpoint: Some(pending.token_endpoint),
        pending: None,
        ..existing
    };
    auth_state
        .servers
        .insert(server_name.to_string(), completed.clone());
    save_auth_state(&auth_state)?;
    clear_cached_runtime_state_for_server(server_name);
    Ok(completed)
}

/// Ensure MCP tool descriptors are cached for all enabled servers.
/// Triggers async discovery for any server whose cache entry is missing or expired.
/// Safe to call multiple times 鈥?hits cache on subsequent calls within the TTL window.
#[cfg(test)]
pub(crate) async fn ensure_tools_cached(config: &Config, workspace: &Path) {
    let _ = list_tools(config, workspace).await;
}

#[allow(dead_code)]
pub(crate) async fn ensure_policy_tools_cached(config: &Config, workspace: &Path) {
    let policy = load_session_policy(workspace);
    let _ = list_tools_for_policy(config, workspace, &policy).await;
}

#[allow(dead_code)]
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

pub(crate) async fn tool_definitions_openai_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Vec<Value> {
    list_tools_for_policy(config, workspace, policy)
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

#[allow(dead_code)]
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

pub(crate) fn cached_tool_definitions_openai_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Vec<Value> {
    cached_list_tools_for_policy(config, workspace, policy)
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

#[allow(dead_code)]
pub(crate) async fn tool_definitions_ollama(config: &Config, workspace: &Path) -> Vec<Value> {
    tool_definitions_openai(config, workspace).await
}

#[allow(dead_code)]
pub(crate) fn cached_tool_definitions_ollama(config: &Config, workspace: &Path) -> Vec<Value> {
    cached_tool_definitions_openai(config, workspace)
}

pub(crate) async fn tool_definitions_ollama_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Vec<Value> {
    tool_definitions_openai_for_policy(config, workspace, policy).await
}

pub(crate) fn cached_tool_definitions_ollama_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Vec<Value> {
    cached_tool_definitions_openai_for_policy(config, workspace, policy)
}

#[allow(dead_code)]
pub(crate) async fn tool_definitions_gemini(config: &Config, workspace: &Path) -> Vec<Value> {
    list_tools(config, workspace)
        .await
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.exposed_name,
                "description": tool.description,
                "parameters": super::gemini_tool_parameters(tool.input_schema),
            })
        })
        .collect()
}

#[allow(dead_code)]
pub(crate) fn cached_tool_definitions_gemini(config: &Config, workspace: &Path) -> Vec<Value> {
    cached_list_tools(config, workspace)
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.exposed_name,
                "description": tool.description,
                "parameters": super::gemini_tool_parameters(tool.input_schema),
            })
        })
        .collect()
}

pub(crate) async fn tool_definitions_gemini_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Vec<Value> {
    list_tools_for_policy(config, workspace, policy)
        .await
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.exposed_name,
                "description": tool.description,
                "parameters": super::gemini_tool_parameters(tool.input_schema),
            })
        })
        .collect()
}

pub(crate) fn cached_tool_definitions_gemini_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Vec<Value> {
    cached_list_tools_for_policy(config, workspace, policy)
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.exposed_name,
                "description": tool.description,
                "parameters": super::gemini_tool_parameters(tool.input_schema),
            })
        })
        .collect()
}

#[allow(dead_code)]
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

pub(crate) async fn tool_definitions_anthropic_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Vec<Value> {
    list_tools_for_policy(config, workspace, policy)
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

#[allow(dead_code)]
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

pub(crate) fn cached_tool_definitions_anthropic_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Vec<Value> {
    cached_list_tools_for_policy(config, workspace, policy)
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

#[cfg(test)]
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

pub(crate) fn cached_server_counts_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> (usize, usize) {
    if policy.enabled_servers.is_empty() || policy.enabled_tools.is_empty() {
        return (0, 0);
    }

    let mut enabled_servers = 0;
    let mut cached_servers = 0;
    let now = Instant::now();

    let servers_with_enabled_tools = policy
        .enabled_servers
        .iter()
        .filter(|server_name| {
            policy
                .enabled_tools
                .iter()
                .any(|tool| exposed_tool_matches_server(tool, server_name))
        })
        .collect::<HashSet<_>>();

    for server_name in servers_with_enabled_tools {
        let Some(server) = config
            .mcp_servers
            .get(server_name)
            .filter(|server| server.enabled)
        else {
            continue;
        };
        enabled_servers += 1;
        let Ok(key) = cache_key(server_name, server, workspace, config) else {
            continue;
        };
        let required_tools = policy
            .enabled_tools
            .iter()
            .filter(|tool| exposed_tool_matches_server(tool, server_name))
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let has_cache = {
            let Ok(mut cache) = tool_cache().lock() else {
                continue;
            };
            let (has_cache, should_remove) = match cache.get(&key) {
                Some(entry) if now.duration_since(entry.loaded_at) < tool_cache_ttl() => {
                    let cached_tools = entry
                        .descriptors
                        .iter()
                        .map(|descriptor| descriptor.exposed_name.as_str())
                        .collect::<HashSet<_>>();
                    let is_complete = required_tools
                        .iter()
                        .all(|tool| cached_tools.contains(tool));
                    (is_complete, !is_complete)
                }
                Some(_) => (false, true),
                None => (false, false),
            };
            if should_remove {
                cache.remove(&key);
            }
            has_cache
        };
        if has_cache {
            cached_servers += 1;
        }
    }

    (cached_servers, enabled_servers)
}

#[allow(dead_code)]
pub(crate) async fn execute_tool(
    name: &str,
    args_str: &str,
    config: &Config,
    workspace: &Path,
) -> Option<ToolOutcome> {
    execute_tool_with_session_mode(name, args_str, config, workspace, false, None).await
}

/// Execute an MCP tool with an isolated per-call session.
/// Used for parallel read-only batches so concurrent calls are not serialized
/// behind the shared cached session mutex.
#[allow(dead_code)]
pub(crate) async fn execute_tool_isolated(
    name: &str,
    args_str: &str,
    config: &Config,
    workspace: &Path,
) -> Option<ToolOutcome> {
    execute_tool_with_session_mode(name, args_str, config, workspace, true, None).await
}

pub(crate) async fn execute_tool_for_policy(
    name: &str,
    args_str: &str,
    config: &Config,
    workspace: &Path,
    isolated_session: bool,
    policy: &McpSessionPolicy,
) -> Option<ToolOutcome> {
    execute_tool_with_session_mode(
        name,
        args_str,
        config,
        workspace,
        isolated_session,
        Some(policy),
    )
    .await
}

async fn execute_tool_with_session_mode(
    name: &str,
    args_str: &str,
    config: &Config,
    workspace: &Path,
    isolated_session: bool,
    policy: Option<&McpSessionPolicy>,
) -> Option<ToolOutcome> {
    if !is_mcp_tool_name(name) {
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
                subagent_snapshot: None,
            });
        }
    };

    if let Some(policy) = policy {
        let allowed_by_name = policy.enabled_tools.contains(name);
        let allowed_by_server = name
            .strip_prefix(MCP_NAME_PREFIX)
            .and_then(|rest| rest.split_once("__"))
            .is_some_and(|(server_segment, _)| {
                policy
                    .enabled_servers
                    .iter()
                    .any(|server_name| sanitize_name_segment(server_name) == server_segment)
            });
        if !allowed_by_name || !allowed_by_server {
            return Some(ToolOutcome {
                output: format!("MCP tool is not enabled for this session: {name}"),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            });
        }
    }

    let descriptor = match if let Some(policy) = policy {
        find_tool_by_exposed_name_for_policy(name, config, workspace, policy).await
    } else {
        find_tool_by_exposed_name(name, config, workspace).await
    } {
        Ok(Some(tool)) => tool,
        Ok(None) => {
            return Some(ToolOutcome {
                output: format!("Unknown MCP tool: {name}"),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            });
        }
        Err(error) => {
            return Some(ToolOutcome {
                output: format!("{name} error: {error}"),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            });
        }
    };

    if let Some(policy) = policy
        && !policy.allows_tool(&descriptor)
    {
        return Some(ToolOutcome {
            output: format!("MCP tool is not enabled for this session: {name}"),
            is_error: true,
            duration_ms: start.elapsed().as_millis() as u64,
            subagent_snapshot: None,
        });
    }

    if let Some(policy) = policy
        && policy.confirm_mutating_tools
        && !is_read_only_tool_descriptor(&descriptor)
    {
        return Some(ToolOutcome {
            output: format!(
                "MCP tool requires confirmation before execution and was blocked: {name}"
            ),
            is_error: true,
            duration_ms: start.elapsed().as_millis() as u64,
            subagent_snapshot: None,
        });
    }

    let call_result = if isolated_session {
        call_server_once(
            &descriptor.server_name,
            config,
            workspace,
            "tools/call",
            json!({
                "name": descriptor.raw_name,
                "arguments": args,
            }),
        )
        .await
    } else {
        call_server(
            &descriptor.server_name,
            config,
            workspace,
            "tools/call",
            json!({
                "name": descriptor.raw_name,
                "arguments": args,
            }),
        )
        .await
    };

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
                subagent_snapshot: None,
            })
        }
        Err(error) => Some(ToolOutcome {
            output: format!("{name} error: {error}"),
            is_error: true,
            duration_ms,
            subagent_snapshot: None,
        }),
    }
}

async fn list_server_catalog_uncached(
    server_name: &str,
    config: &Config,
    workspace: &Path,
) -> McpServerCatalogLoad {
    let mut successful_lists = 0;
    let mut errors = Vec::new();

    let (tools, tools_loaded) =
        match list_server_tools_uncached(server_name, config, workspace).await {
            Ok(tools) => {
                successful_lists += 1;
                (tools, true)
            }
            Err(error) => {
                errors.push(format!("tools/list: {error}"));
                (Vec::new(), false)
            }
        };
    let (resources, resources_loaded) =
        match list_server_resources_uncached(server_name, config, workspace).await {
            Ok(resources) => {
                successful_lists += 1;
                (resources, true)
            }
            Err(error) => {
                errors.push(format!("resources/list: {error}"));
                (Vec::new(), false)
            }
        };
    let (prompts, prompts_loaded) =
        match list_server_prompts_uncached(server_name, config, workspace).await {
            Ok(prompts) => {
                successful_lists += 1;
                (prompts, true)
            }
            Err(error) => {
                errors.push(format!("prompts/list: {error}"));
                (Vec::new(), false)
            }
        };

    let error = if successful_lists == 0 && !errors.is_empty() {
        Some(errors.join("; "))
    } else {
        None
    };
    McpServerCatalogLoad {
        tools,
        resources,
        prompts,
        tools_loaded,
        resources_loaded,
        prompts_loaded,
        error,
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
        let catalog = list_server_catalog_uncached(server_name, config, workspace).await;
        McpServerLoadReport {
            server_name: server_name.to_string(),
            transport: config
                .mcp_servers
                .get(server_name)
                .map(JsonMcpServerConfig::effective_transport)
                .unwrap_or_else(|| "stdio".to_string()),
            tool_names: catalog
                .tools
                .into_iter()
                .map(|tool| tool.exposed_name)
                .collect(),
            resource_count: catalog.resources.len(),
            prompt_count: catalog.prompts.len(),
            error: catalog.error,
        }
    }))
    .await
}

pub(crate) async fn catalog_snapshot(config: &Config, workspace: &Path) -> McpCatalogSnapshot {
    let mut server_names: Vec<&str> = config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, _)| name.as_str())
        .collect();
    server_names.sort_unstable();

    let results = join_all(server_names.into_iter().map(|server_name| async move {
        let transport = config
            .mcp_servers
            .get(server_name)
            .map(JsonMcpServerConfig::effective_transport)
            .unwrap_or_else(|| "stdio".to_string());

        let catalog = list_server_catalog_uncached(server_name, config, workspace).await;
        let report = McpServerLoadReport {
            server_name: server_name.to_string(),
            transport,
            tool_names: catalog
                .tools
                .iter()
                .map(|tool| tool.exposed_name.clone())
                .collect(),
            resource_count: catalog.resources.len(),
            prompt_count: catalog.prompts.len(),
            error: catalog.error.clone(),
        };
        (catalog.tools, catalog.resources, catalog.prompts, report)
    }))
    .await;

    let mut snapshot = McpCatalogSnapshot::default();
    for (mut tools, mut resources, mut prompts, report) in results {
        snapshot.tools.append(&mut tools);
        snapshot.resources.append(&mut resources);
        snapshot.prompts.append(&mut prompts);
        snapshot.reports.push(report);
    }
    snapshot
}

/// Test a single MCP server by spawning it, running tools/list, and returning the tool count.
/// Uses a temporary Config with just the one server so it does not require a pre-existing config.
pub(crate) async fn test_mcp_server(
    server_name: &str,
    mcp_cfg: &JsonMcpServerConfig,
    workspace: &Path,
    default_tool_timeout: Duration,
) -> Result<usize, String> {
    let server_name = server_name.trim();
    let server_name = if server_name.is_empty() {
        "__test__"
    } else {
        server_name
    };
    let mut mcp_servers = HashMap::new();
    mcp_servers.insert(server_name.to_string(), mcp_cfg.clone());
    let temp_config = Config {
        api_key: String::new(),
        api_base: String::new(),
        model: String::new(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,
        reflection_model: None,
        context_model: None,
        provider: crate::Provider::OpenAI,
        openai_stream_include_usage: false,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers,
        port: 0,
        max_context_tokens: 4096,
        exec_timeout: Duration::from_secs(30),
        tool_timeout: Duration::from_secs(
            mcp_cfg
                .timeout_secs
                .unwrap_or(default_tool_timeout.as_secs()),
        ),
        sub_agent_timeout: Duration::from_secs(300),
        max_llm_retries: 1,
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
        structured_memory: false,
        daily_reflection: false,
        enable_state_digest: true,
        s3: None,
    };
    let tools = list_server_tools_uncached(server_name, &temp_config, workspace).await?;
    Ok(tools.len())
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
        let Some(server) = config.mcp_servers.get(server_name) else {
            return Ok(McpServerLoadReport {
                server_name: server_name.to_string(),
                transport: "stdio".to_string(),
                tool_names: Vec::new(),
                resource_count: 0,
                prompt_count: 0,
                error: Some(format!("unknown MCP server '{server_name}'")),
            });
        };
        let catalog = list_server_catalog_uncached(server_name, config, workspace).await;
        match cache_key(server_name, server, workspace, config) {
            Ok(cache_key) => {
                let now = Instant::now();
                if catalog.tools_loaded {
                    {
                        let mut cache = tool_cache()
                            .lock()
                            .map_err(|_| "MCP tool cache lock poisoned".to_string())?;
                        cache.insert(
                            cache_key.clone(),
                            CachedToolDescriptors {
                                descriptors: catalog.tools.clone(),
                                loaded_at: now,
                            },
                        );
                    }
                }
                if catalog.resources_loaded {
                    {
                        let mut cache = resource_cache()
                            .lock()
                            .map_err(|_| "MCP resource cache lock poisoned".to_string())?;
                        cache.insert(
                            cache_key.clone(),
                            CachedResourceDescriptors {
                                descriptors: catalog.resources.clone(),
                                loaded_at: now,
                            },
                        );
                    }
                }
                if catalog.prompts_loaded {
                    {
                        let mut cache = prompt_cache()
                            .lock()
                            .map_err(|_| "MCP prompt cache lock poisoned".to_string())?;
                        cache.insert(
                            cache_key,
                            CachedPromptDescriptors {
                                descriptors: catalog.prompts.clone(),
                                loaded_at: now,
                            },
                        );
                    }
                }
                Ok(McpServerLoadReport {
                    server_name: server_name.to_string(),
                    transport: server.effective_transport(),
                    tool_names: catalog
                        .tools
                        .into_iter()
                        .map(|tool| tool.exposed_name)
                        .collect(),
                    resource_count: catalog.resources.len(),
                    prompt_count: catalog.prompts.len(),
                    error: catalog.error,
                })
            }
            Err(error) => Ok(McpServerLoadReport {
                server_name: server_name.to_string(),
                transport: server.effective_transport(),
                tool_names: Vec::new(),
                resource_count: 0,
                prompt_count: 0,
                error: Some(error),
            }),
        }
    }))
    .await;

    results.into_iter().collect()
}

pub(crate) async fn invalidate_runtime_state_without_remote_shutdown() {
    if let Ok(mut cache) = tool_cache().lock() {
        cache.clear();
    }
    if let Ok(mut cache) = resource_cache().lock() {
        cache.clear();
    }
    if let Ok(mut cache) = prompt_cache().lock() {
        cache.clear();
    }
    if let Ok(mut cache) = http_session_cache().lock() {
        cache.clear();
    }
    if let Ok(mut cache) = http_last_event_ids().lock() {
        cache.clear();
    }
    if let Ok(mut locks) = http_initialization_locks().lock() {
        locks.clear();
    }
    if let Ok(mut failures) = spawn_failures().lock() {
        failures.clear();
    }

    let stream_tasks = {
        match http_stream_tasks().lock() {
            Ok(mut tasks) => tasks
                .drain()
                .map(|(_, entry)| entry.handle)
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        }
    };
    for task in stream_tasks {
        task.abort();
    }

    let sessions = {
        match session_cache().lock() {
            Ok(mut cache) => cache
                .drain()
                .map(|(_, cached)| cached.session)
                .collect::<Vec<_>>(),
            Err(_) => Vec::new(),
        }
    };
    for session in sessions {
        let mut guard = session.lock().await;
        guard.shutdown().await;
    }
}

fn tool_cache() -> &'static Mutex<HashMap<String, CachedToolDescriptors>> {
    MCP_TOOL_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_cache() -> &'static Mutex<HashMap<String, CachedMcpSession>> {
    MCP_SESSION_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn resource_cache() -> &'static Mutex<HashMap<String, CachedResourceDescriptors>> {
    MCP_RESOURCE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn prompt_cache() -> &'static Mutex<HashMap<String, CachedPromptDescriptors>> {
    MCP_PROMPT_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn http_session_cache() -> &'static Mutex<HashMap<String, CachedHttpMcpSession>> {
    MCP_HTTP_SESSION_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn http_stream_tasks() -> &'static Mutex<HashMap<String, HttpStreamTaskEntry>> {
    MCP_HTTP_STREAM_TASKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_http_stream_task_id() -> u64 {
    MCP_NEXT_HTTP_STREAM_TASK_ID.fetch_add(1, Ordering::Relaxed)
}

fn next_http_request_id() -> u64 {
    MCP_NEXT_HTTP_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

fn http_last_event_ids() -> &'static Mutex<HashMap<String, String>> {
    MCP_HTTP_LAST_EVENT_IDS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn http_initialization_locks() -> &'static Mutex<HashMap<String, Arc<AsyncMutex<()>>>> {
    MCP_HTTP_INITIALIZATION_LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn http_initialization_lock(cache_key: &str) -> Arc<AsyncMutex<()>> {
    match http_initialization_locks().lock() {
        Ok(mut locks) => locks
            .entry(cache_key.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone(),
        Err(_) => Arc::new(AsyncMutex::new(())),
    }
}

fn is_streamable_http_server(server: &JsonMcpServerConfig) -> bool {
    server.effective_transport() == "streamable-http"
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

pub(crate) fn exposed_tool_matches_server(tool_name: &str, server_name: &str) -> bool {
    tool_name
        .strip_prefix(MCP_NAME_PREFIX)
        .and_then(|rest| rest.split_once("__"))
        .is_some_and(|(server_segment, _)| server_segment == sanitize_name_segment(server_name))
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

async fn list_tools_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Vec<McpToolDescriptor> {
    if policy.enabled_servers.is_empty() || policy.enabled_tools.is_empty() {
        return Vec::new();
    }

    let mut server_names: Vec<&str> = policy
        .enabled_servers
        .iter()
        .filter_map(|server_name| {
            config
                .mcp_servers
                .get(server_name)
                .filter(|server| server.enabled)
                .map(|_| server_name.as_str())
        })
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
        .into_iter()
        .filter(|tool| policy.allows_tool(tool))
        .collect()
}

#[allow(dead_code)]
pub(crate) async fn list_tools_for_servers(
    config: &Config,
    workspace: &Path,
    server_names: &HashSet<String>,
) -> Vec<McpToolDescriptor> {
    list_tools_for_servers_with_status(config, workspace, server_names)
        .await
        .0
}

#[allow(dead_code)]
pub(crate) async fn list_tools_for_servers_with_status(
    config: &Config,
    workspace: &Path,
    server_names: &HashSet<String>,
) -> (Vec<McpToolDescriptor>, HashSet<String>) {
    list_tools_for_servers_with_status_inner(config, workspace, server_names, false).await
}

pub(crate) async fn list_tools_for_servers_uncached_with_status(
    config: &Config,
    workspace: &Path,
    server_names: &HashSet<String>,
) -> (Vec<McpToolDescriptor>, HashSet<String>) {
    list_tools_for_servers_with_status_inner(config, workspace, server_names, true).await
}

async fn list_tools_for_servers_with_status_inner(
    config: &Config,
    workspace: &Path,
    server_names: &HashSet<String>,
    uncached: bool,
) -> (Vec<McpToolDescriptor>, HashSet<String>) {
    if server_names.is_empty() {
        return (Vec::new(), HashSet::new());
    }

    let mut names: Vec<&str> = server_names
        .iter()
        .filter_map(|server_name| {
            config
                .mcp_servers
                .get(server_name)
                .filter(|server| server.enabled)
                .map(|_| server_name.as_str())
        })
        .collect();
    names.sort_unstable();

    let results = join_all(names.into_iter().map(|server_name| async move {
        let result = if uncached {
            list_server_tools_uncached(server_name, config, workspace).await
        } else {
            list_server_tools(server_name, config, workspace).await
        };
        (server_name, result)
    }))
    .await;

    let mut tools = Vec::new();
    let mut successful_servers = HashSet::new();
    for (server_name, result) in results {
        match result {
            Ok(mut server_tools) => {
                successful_servers.insert(server_name.to_string());
                tools.append(&mut server_tools);
            }
            Err(error) => eprintln!("Warning: MCP server '{server_name}' unavailable: {error}"),
        }
    }
    (tools, successful_servers)
}

pub(crate) fn cached_list_tools(config: &Config, workspace: &Path) -> Vec<McpToolDescriptor> {
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

pub(crate) fn cached_list_tools_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Vec<McpToolDescriptor> {
    if policy.enabled_servers.is_empty() || policy.enabled_tools.is_empty() {
        return Vec::new();
    }
    cached_list_tools(config, workspace)
        .into_iter()
        .filter(|tool| policy.allows_tool(tool))
        .collect()
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
    let mut header_items: Vec<String> = server
        .headers
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    header_items.sort_unstable();
    let capabilities =
        initialize_capabilities(&client_capabilities_for_server(server_name, workspace));
    let capabilities_key =
        serde_json::to_string(&capabilities).unwrap_or_else(|_| "{}".to_string());
    Ok(format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        server_name,
        server.effective_transport(),
        server.command,
        server.url.as_deref().unwrap_or_default(),
        server.args.join("\u{1f}"),
        resolved_cwd.display(),
        server_timeout_secs(server, config),
        env_items.join("\u{1f}"),
        header_items.join("\u{1f}"),
        capabilities_key
    ))
}

async fn find_tool_by_exposed_name(
    name: &str,
    config: &Config,
    workspace: &Path,
) -> Result<Option<McpToolDescriptor>, String> {
    find_tool_by_exposed_name_filtered(name, config, workspace, None).await
}

async fn find_tool_by_exposed_name_for_policy(
    name: &str,
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Result<Option<McpToolDescriptor>, String> {
    find_tool_by_exposed_name_filtered(name, config, workspace, Some(policy)).await
}

async fn find_tool_by_exposed_name_filtered(
    name: &str,
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
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
        .filter(|(server_name, _)| {
            policy.is_none_or(|policy| policy.enabled_servers.contains(server_name.as_str()))
        })
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

    let tools = list_server_items(
        server_name,
        config,
        workspace,
        "tools/list",
        "tools",
        json!({}),
        false,
    )
    .await?;
    let descriptors = parse_tool_descriptors(server_name, &json!({ "tools": tools }))?;

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
    let tools = list_server_items(
        server_name,
        config,
        workspace,
        "tools/list",
        "tools",
        json!({}),
        true,
    )
    .await?;
    parse_tool_descriptors(server_name, &json!({ "tools": tools }))
}

async fn list_server_items(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    method: &str,
    array_key: &str,
    base_params: Value,
    uncached_session: bool,
) -> Result<Vec<Value>, String> {
    if uncached_session {
        return list_server_items_with_temporary_session(
            server_name,
            config,
            workspace,
            method,
            array_key,
            base_params,
        )
        .await;
    }

    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    let mut items = Vec::new();

    for page_index in 0..MCP_MAX_PAGINATION_PAGES {
        let mut params = base_params.clone();
        if let Some(cursor) = cursor.as_deref() {
            match &mut params {
                Value::Object(map) => {
                    map.insert("cursor".to_string(), json!(cursor));
                }
                _ => {
                    params = json!({ "cursor": cursor });
                }
            }
        }
        let response = call_server(server_name, config, workspace, method, params).await?;
        let page = response
            .get(array_key)
            .and_then(Value::as_array)
            .ok_or_else(|| format!("server '{server_name}' returned invalid {method} payload"))?;
        items.extend(page.iter().cloned());

        cursor = response
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty());
        let Some(next_cursor) = cursor.as_ref() else {
            return Ok(items);
        };
        if !seen_cursors.insert(next_cursor.clone()) {
            return Err(format!(
                "server '{server_name}' returned a repeated pagination cursor for {method}"
            ));
        }
        if page_index + 1 == MCP_MAX_PAGINATION_PAGES {
            return Err(format!(
                "server '{server_name}' exceeded {MCP_MAX_PAGINATION_PAGES} pages for {method}"
            ));
        }
    }

    Err(format!(
        "server '{server_name}' exceeded {MCP_MAX_PAGINATION_PAGES} pages for {method}"
    ))
}

async fn list_server_items_with_temporary_session(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    method: &str,
    array_key: &str,
    base_params: Value,
) -> Result<Vec<Value>, String> {
    let mut session = TemporaryMcpSession::new(server_name, config, workspace).await?;
    let result = async {
        let mut cursor: Option<String> = None;
        let mut seen_cursors = HashSet::new();
        let mut items = Vec::new();

        for page_index in 0..MCP_MAX_PAGINATION_PAGES {
            let mut params = base_params.clone();
            if let Some(cursor) = cursor.as_deref() {
                match &mut params {
                    Value::Object(map) => {
                        map.insert("cursor".to_string(), json!(cursor));
                    }
                    _ => {
                        params = json!({ "cursor": cursor });
                    }
                }
            }
            let response = session.request(workspace, method, params).await?;
            let page = response
                .get(array_key)
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    format!("server '{server_name}' returned invalid {method} payload")
                })?;
            items.extend(page.iter().cloned());

            cursor = response
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string)
                .filter(|value| !value.is_empty());
            let Some(next_cursor) = cursor.as_ref() else {
                return Ok(items);
            };
            if !seen_cursors.insert(next_cursor.clone()) {
                return Err(format!(
                    "server '{server_name}' returned a repeated pagination cursor for {method}"
                ));
            }
            if page_index + 1 == MCP_MAX_PAGINATION_PAGES {
                return Err(format!(
                    "server '{server_name}' exceeded {MCP_MAX_PAGINATION_PAGES} pages for {method}"
                ));
            }
        }

        Err(format!(
            "server '{server_name}' exceeded {MCP_MAX_PAGINATION_PAGES} pages for {method}"
        ))
    }
    .await;
    session.shutdown().await;
    result
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

#[allow(dead_code)]
pub(crate) async fn list_resources(
    config: &Config,
    workspace: &Path,
) -> Vec<McpResourceDescriptor> {
    let mut server_names: Vec<&str> = config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, _)| name.as_str())
        .collect();
    server_names.sort_unstable();

    let results = join_all(server_names.into_iter().map(|server_name| async move {
        list_server_resources(server_name, config, workspace).await
    }))
    .await;
    results
        .into_iter()
        .filter_map(Result::ok)
        .flatten()
        .collect()
}

#[allow(dead_code)]
pub(crate) async fn list_prompts(config: &Config, workspace: &Path) -> Vec<McpPromptDescriptor> {
    let mut server_names: Vec<&str> = config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, _)| name.as_str())
        .collect();
    server_names.sort_unstable();

    let results = join_all(server_names.into_iter().map(|server_name| async move {
        list_server_prompts(server_name, config, workspace).await
    }))
    .await;
    results
        .into_iter()
        .filter_map(Result::ok)
        .flatten()
        .collect()
}

#[allow(dead_code)]
async fn list_server_resources(
    server_name: &str,
    config: &Config,
    workspace: &Path,
) -> Result<Vec<McpResourceDescriptor>, String> {
    let server = config
        .mcp_servers
        .get(server_name)
        .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
    let key = cache_key(server_name, server, workspace, config)?;
    let now = Instant::now();

    let cached = {
        let mut cache = resource_cache()
            .lock()
            .map_err(|_| "MCP resource cache lock poisoned".to_string())?;
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

    let resources = list_server_items(
        server_name,
        config,
        workspace,
        "resources/list",
        "resources",
        json!({}),
        false,
    )
    .await?;
    let descriptors = parse_resource_descriptors(server_name, &json!({ "resources": resources }))?;
    {
        let mut cache = resource_cache()
            .lock()
            .map_err(|_| "MCP resource cache lock poisoned".to_string())?;
        cache.insert(
            key,
            CachedResourceDescriptors {
                descriptors: descriptors.clone(),
                loaded_at: Instant::now(),
            },
        );
    }
    Ok(descriptors)
}

async fn list_server_resources_uncached(
    server_name: &str,
    config: &Config,
    workspace: &Path,
) -> Result<Vec<McpResourceDescriptor>, String> {
    let resources = list_server_items(
        server_name,
        config,
        workspace,
        "resources/list",
        "resources",
        json!({}),
        true,
    )
    .await?;
    parse_resource_descriptors(server_name, &json!({ "resources": resources }))
}

#[allow(dead_code)]
async fn list_server_prompts(
    server_name: &str,
    config: &Config,
    workspace: &Path,
) -> Result<Vec<McpPromptDescriptor>, String> {
    let server = config
        .mcp_servers
        .get(server_name)
        .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
    let key = cache_key(server_name, server, workspace, config)?;
    let now = Instant::now();

    let cached = {
        let mut cache = prompt_cache()
            .lock()
            .map_err(|_| "MCP prompt cache lock poisoned".to_string())?;
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

    let prompts = list_server_items(
        server_name,
        config,
        workspace,
        "prompts/list",
        "prompts",
        json!({}),
        false,
    )
    .await?;
    let descriptors = parse_prompt_descriptors(server_name, &json!({ "prompts": prompts }))?;
    {
        let mut cache = prompt_cache()
            .lock()
            .map_err(|_| "MCP prompt cache lock poisoned".to_string())?;
        cache.insert(
            key,
            CachedPromptDescriptors {
                descriptors: descriptors.clone(),
                loaded_at: Instant::now(),
            },
        );
    }
    Ok(descriptors)
}

async fn list_server_prompts_uncached(
    server_name: &str,
    config: &Config,
    workspace: &Path,
) -> Result<Vec<McpPromptDescriptor>, String> {
    let prompts = list_server_items(
        server_name,
        config,
        workspace,
        "prompts/list",
        "prompts",
        json!({}),
        true,
    )
    .await?;
    parse_prompt_descriptors(server_name, &json!({ "prompts": prompts }))
}

fn parse_resource_descriptors(
    server_name: &str,
    response: &Value,
) -> Result<Vec<McpResourceDescriptor>, String> {
    let resources = response
        .get("resources")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("server '{server_name}' returned invalid resources/list payload"))?;

    Ok(resources
        .iter()
        .filter_map(|resource| {
            let uri = resource.get("uri").and_then(Value::as_str)?.to_string();
            let name = resource
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&uri)
                .to_string();
            let description = resource
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let mime_type = resource
                .get("mimeType")
                .or_else(|| resource.get("mime_type"))
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(McpResourceDescriptor {
                server_name: server_name.to_string(),
                uri,
                name,
                description,
                mime_type,
            })
        })
        .collect())
}

fn parse_prompt_descriptors(
    server_name: &str,
    response: &Value,
) -> Result<Vec<McpPromptDescriptor>, String> {
    let prompts = response
        .get("prompts")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("server '{server_name}' returned invalid prompts/list payload"))?;

    Ok(prompts
        .iter()
        .filter_map(|prompt| {
            let raw_name = prompt.get("name").and_then(Value::as_str)?.to_string();
            let description = prompt
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments = prompt
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!([]));
            Some(McpPromptDescriptor {
                server_name: server_name.to_string(),
                raw_name,
                description,
                arguments,
            })
        })
        .collect())
}

pub(crate) async fn read_resource(
    server_name: &str,
    uri: &str,
    config: &Config,
    workspace: &Path,
) -> Result<Value, String> {
    call_server(
        server_name,
        config,
        workspace,
        "resources/read",
        json!({ "uri": uri }),
    )
    .await
}

pub(crate) async fn get_prompt(
    server_name: &str,
    name: &str,
    arguments: Value,
    config: &Config,
    workspace: &Path,
) -> Result<Value, String> {
    call_server(
        server_name,
        config,
        workspace,
        "prompts/get",
        json!({ "name": name, "arguments": arguments }),
    )
    .await
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
    {
        let mut cache = resource_cache()
            .lock()
            .map_err(|_| "MCP resource cache lock poisoned".to_string())?;
        for cache_key in &cache_keys {
            cache.remove(cache_key);
        }
    }
    {
        let mut cache = prompt_cache()
            .lock()
            .map_err(|_| "MCP prompt cache lock poisoned".to_string())?;
        for cache_key in &cache_keys {
            cache.remove(cache_key);
        }
    }
    for (server_name, server) in config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled && is_streamable_http_server(server))
    {
        if let Ok(cache_key) = cache_key(server_name, server, workspace, config) {
            terminate_http_session(server_name, &cache_key, server).await;
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
        let capabilities = initialize_capabilities(&self.client_capabilities);
        write_message(
            &mut self.stdin,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": capabilities,
                    "clientInfo": {
                        "name": "LingClaw",
                        "version": VERSION,
                    }
                }
            }),
        )
        .await?;
        let initialize = match tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            read_response(
                &mut self.reader,
                &mut self.stdin,
                1,
                &self.stdout_lines,
                &self.server_name,
                &self.workspace_root,
                &self.tool_cache_key,
                &self.client_capabilities,
            ),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                let _ = write_message(
                    &mut self.stdin,
                    &json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/cancelled",
                        "params": {
                            "requestId": 1,
                            "reason": "initialize timed out"
                        }
                    }),
                )
                .await;
                return Err(format_mcp_timeout_error(
                    "initialize",
                    self.timeout_secs,
                    &snapshot_diagnostic_lines(&self.stdout_lines),
                    &snapshot_diagnostic_lines(&self.stderr_lines),
                ));
            }
        };
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

        let response = match tokio::time::timeout(
            Duration::from_secs(self.timeout_secs),
            read_response(
                &mut self.reader,
                &mut self.stdin,
                request_id,
                &self.stdout_lines,
                &self.server_name,
                &self.workspace_root,
                &self.tool_cache_key,
                &self.client_capabilities,
            ),
        )
        .await
        {
            Ok(result) => result?,
            Err(_) => {
                let _ = write_message(
                    &mut self.stdin,
                    &json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/cancelled",
                        "params": {
                            "requestId": request_id,
                            "reason": format!("{method} timed out")
                        }
                    }),
                )
                .await;
                return Err(format_mcp_timeout_error(
                    method,
                    self.timeout_secs,
                    &snapshot_diagnostic_lines(&self.stdout_lines),
                    &snapshot_diagnostic_lines(&self.stderr_lines),
                ));
            }
        };

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
        if let Some(mut stderr_task) = self.stderr_task.take() {
            stderr_task.abort();
            let _ = (&mut stderr_task).await;
        }
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

fn apply_mcp_process_flags(command: &mut Command) {
    #[cfg(target_os = "windows")]
    {
        // Keep console-style MCP helpers such as `uvx.exe` attached to pipes
        // without flashing a separate terminal window for every tool call.
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = command;
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
    let client_capabilities = client_capabilities_for_server(server_name, workspace);
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
    apply_mcp_process_flags(&mut command);
    for (key, value) in &server.env {
        command.env(key, resolve_env_placeholder(value));
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
        client_capabilities,
        timeout_secs: server_timeout_secs(server, config),
        next_request_id: 2,
        child,
        stdin,
        reader: BufReader::new(stdout),
        stderr_task: Some(stderr_task),
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

fn resolve_env_placeholder(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut rest = value;

    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };

        let name = &after_open[..end];
        let valid_name = !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
        if valid_name {
            match std::env::var(name) {
                Ok(replacement) => output.push_str(&replacement),
                Err(_) => {
                    output.push_str("${");
                    output.push_str(name);
                    output.push('}');
                }
            }
        } else {
            output.push_str("${");
            output.push_str(name);
            output.push('}');
        }

        rest = &after_open[end + 1..];
    }

    output.push_str(rest);
    output
}

fn token_needs_refresh(expires_at: Option<u64>) -> bool {
    expires_at.is_some_and(|expires_at| expires_at <= now_unix_secs().saturating_add(60))
}

async fn refresh_bearer_token(
    server_name: &str,
    existing: McpServerAuthState,
    timeout_secs: u64,
) -> Result<String, String> {
    let refresh_token = existing
        .refresh_token
        .clone()
        .ok_or_else(|| format!("OAuth token for MCP server '{server_name}' expired; reconnect"))?;
    let token_endpoint = existing.token_endpoint.clone().ok_or_else(|| {
        format!("OAuth token for MCP server '{server_name}' has no token endpoint")
    })?;
    let client_id = existing
        .client_id
        .clone()
        .ok_or_else(|| format!("OAuth token for MCP server '{server_name}' has no client id"))?;

    let mut form = vec![
        ("grant_type".to_string(), "refresh_token".to_string()),
        ("refresh_token".to_string(), refresh_token),
        ("client_id".to_string(), client_id),
    ];
    if let Some(secret) = existing.client_secret.as_deref() {
        form.push(("client_secret".to_string(), secret.to_string()));
    }
    if let Some(resource) = existing.resource.as_deref() {
        form.push(("resource".to_string(), resource.to_string()));
    }
    if !existing.scopes.is_empty() {
        form.push(("scope".to_string(), existing.scopes.join(" ")));
    }

    let request = reqwest_client_with_timeout(timeout_secs)?
        .post(&token_endpoint)
        .form(&form);
    let response =
        send_http_request_with_timeout(request, timeout_secs, "OAuth token refresh").await?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response_text_with_timeout(
            response,
            timeout_secs,
            "failed to read OAuth refresh error response",
        )
        .await
        .unwrap_or_default();
        return Err(format!("OAuth token refresh failed with {status}: {text}"));
    }
    let payload =
        response_json_with_timeout::<Value>(response, timeout_secs, "OAuth refresh response")
            .await?;
    let access_token = payload
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "OAuth refresh response missing access_token".to_string())?;

    let mut updated = existing;
    updated.access_token = Some(access_token.clone());
    if let Some(refresh_token) = payload.get("refresh_token").and_then(Value::as_str) {
        updated.refresh_token = Some(refresh_token.to_string());
    }
    updated.expires_at = payload
        .get("expires_in")
        .and_then(Value::as_u64)
        .map(|expires_in| now_unix_secs().saturating_add(expires_in));
    if let Some(scope) = payload.get("scope").and_then(Value::as_str) {
        updated.scopes = scope.split_whitespace().map(str::to_string).collect();
    }
    updated.pending = None;

    let mut auth_state = load_auth_state();
    auth_state.servers.insert(server_name.to_string(), updated);
    save_auth_state(&auth_state)?;
    Ok(access_token)
}

#[cfg(test)]
async fn bearer_token_for_server(
    server_name: &str,
    timeout_secs: u64,
) -> Result<Option<String>, String> {
    let Some(state) = load_auth_state().servers.get(server_name).cloned() else {
        return Ok(None);
    };
    match state.access_token.clone() {
        Some(access_token) if !token_needs_refresh(state.expires_at) => Ok(Some(access_token)),
        _ if state.refresh_token.is_some() => {
            refresh_bearer_token(server_name, state, timeout_secs)
                .await
                .map(Some)
        }
        Some(_) => Err(format!(
            "OAuth token for MCP server '{server_name}' expired; reconnect"
        )),
        None => Ok(None),
    }
}

fn trim_url_slashes(value: &str) -> &str {
    value.trim().trim_end_matches('/')
}

fn oauth_resource_matches_server(server: &JsonMcpServerConfig, resource: &str) -> bool {
    let resource = resource.trim();
    if resource.is_empty() {
        return false;
    }
    let Some(server_url_text) = server
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return false;
    };

    let Ok(server_url) = reqwest::Url::parse(server_url_text) else {
        return trim_url_slashes(resource) == trim_url_slashes(server_url_text);
    };
    let Ok(resource_url) = reqwest::Url::parse(resource) else {
        return trim_url_slashes(resource) == trim_url_slashes(server_url_text);
    };

    if server_url.scheme() != resource_url.scheme()
        || server_url.host_str() != resource_url.host_str()
        || server_url.port_or_known_default() != resource_url.port_or_known_default()
    {
        return false;
    }

    let resource_path = resource_url.path().trim_end_matches('/');
    if resource_path.is_empty() {
        return true;
    }
    server_url.path().trim_end_matches('/') == resource_path
}

fn validate_bearer_token_binding(
    server_name: &str,
    server: &JsonMcpServerConfig,
    state: &McpServerAuthState,
) -> Result<(), String> {
    let Some(resource) = state.resource.as_deref() else {
        return Ok(());
    };
    if oauth_resource_matches_server(server, resource) {
        Ok(())
    } else {
        Err(format!(
            "OAuth token for MCP server '{server_name}' was issued for a different resource; reconnect"
        ))
    }
}

pub(crate) fn auth_state_usable_for_server(
    server_name: &str,
    server: &JsonMcpServerConfig,
    state: &McpServerAuthState,
) -> bool {
    if !state
        .access_token
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || validate_bearer_token_binding(server_name, server, state).is_err()
    {
        return false;
    }
    if !token_needs_refresh(state.expires_at) {
        return true;
    }
    state
        .refresh_token
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && state
            .token_endpoint
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        && state
            .client_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

async fn bearer_token_for_server_config(
    server_name: &str,
    server: &JsonMcpServerConfig,
    timeout_secs: u64,
) -> Result<Option<String>, String> {
    let Some(state) = load_auth_state().servers.get(server_name).cloned() else {
        return Ok(None);
    };
    validate_bearer_token_binding(server_name, server, &state)?;
    match state.access_token.clone() {
        Some(access_token) if !token_needs_refresh(state.expires_at) => Ok(Some(access_token)),
        _ if state.refresh_token.is_some() => {
            refresh_bearer_token(server_name, state, timeout_secs)
                .await
                .map(Some)
        }
        Some(_) => Err(format!(
            "OAuth token for MCP server '{server_name}' expired; reconnect"
        )),
        None => Ok(None),
    }
}

fn http_session_id(cache_key: &str) -> Option<String> {
    let expired = {
        let mut cache = http_session_cache().lock().ok()?;
        let entry = cache.get_mut(cache_key)?;
        if entry.last_used_at.elapsed() >= session_idle_ttl() {
            true
        } else {
            entry.last_used_at = Instant::now();
            return entry.session_id.clone();
        }
    };
    if expired {
        remove_http_session(cache_key);
    }
    None
}

fn set_http_session_id(cache_key: &str, session_id: Option<String>) {
    if let Ok(mut cache) = http_session_cache().lock() {
        cache.insert(
            cache_key.to_string(),
            CachedHttpMcpSession {
                session_id,
                last_used_at: Instant::now(),
            },
        );
    }
}

fn remove_http_session_state(cache_key: &str) {
    if let Ok(mut cache) = http_session_cache().lock() {
        cache.remove(cache_key);
    }
    if let Ok(mut last_event_ids) = http_last_event_ids().lock() {
        last_event_ids.remove(cache_key);
    }
}

fn remove_http_session(cache_key: &str) {
    remove_http_session_state(cache_key);
    if let Ok(mut tasks) = http_stream_tasks().lock()
        && let Some(entry) = tasks.remove(cache_key)
    {
        entry.handle.abort();
    }
}

fn cache_key_belongs_to_server(cache_key: &str, server_name: &str) -> bool {
    cache_key.lines().next() == Some(server_name)
}

fn cached_http_session_keys_for_server(server_name: &str) -> Vec<String> {
    let mut keys = HashSet::new();
    if let Ok(cache) = http_session_cache().lock() {
        keys.extend(
            cache
                .keys()
                .filter(|key| cache_key_belongs_to_server(key, server_name))
                .cloned(),
        );
    }
    if let Ok(tasks) = http_stream_tasks().lock() {
        keys.extend(
            tasks
                .keys()
                .filter(|key| cache_key_belongs_to_server(key, server_name))
                .cloned(),
        );
    }
    if let Ok(last_event_ids) = http_last_event_ids().lock() {
        keys.extend(
            last_event_ids
                .keys()
                .filter(|key| cache_key_belongs_to_server(key, server_name))
                .cloned(),
        );
    }
    keys.into_iter().collect()
}

fn clear_descriptor_caches_for_server(server_name: &str) {
    if let Ok(mut cache) = tool_cache().lock() {
        cache.retain(|key, _| !cache_key_belongs_to_server(key, server_name));
    }
    if let Ok(mut cache) = resource_cache().lock() {
        cache.retain(|key, _| !cache_key_belongs_to_server(key, server_name));
    }
    if let Ok(mut cache) = prompt_cache().lock() {
        cache.retain(|key, _| !cache_key_belongs_to_server(key, server_name));
    }
}

pub(crate) async fn terminate_http_sessions_for_server(
    server_name: &str,
    server: &JsonMcpServerConfig,
) {
    for cache_key in cached_http_session_keys_for_server(server_name) {
        terminate_http_session(server_name, &cache_key, server).await;
    }
}

pub(crate) fn clear_cached_runtime_state_for_server(server_name: &str) {
    clear_descriptor_caches_for_server(server_name);
    if let Ok(mut locks) = http_initialization_locks().lock() {
        locks.retain(|cache_key, _| !cache_key_belongs_to_server(cache_key, server_name));
    }
    for cache_key in cached_http_session_keys_for_server(server_name) {
        remove_http_session(&cache_key);
    }
}

fn http_last_event_id(cache_key: &str) -> Option<String> {
    http_last_event_ids()
        .lock()
        .ok()
        .and_then(|ids| ids.get(cache_key).cloned())
}

fn set_http_last_event_id(cache_key: &str, event_id: &str) {
    if let Ok(mut ids) = http_last_event_ids().lock() {
        ids.insert(cache_key.to_string(), event_id.to_string());
    }
}

struct HttpStreamTaskCleanup {
    cache_key: String,
    task_id: u64,
}

impl Drop for HttpStreamTaskCleanup {
    fn drop(&mut self) {
        if let Ok(mut tasks) = http_stream_tasks().lock() {
            let should_remove = tasks
                .get(&self.cache_key)
                .is_some_and(|entry| entry.task_id == self.task_id);
            if should_remove {
                tasks.remove(&self.cache_key);
            }
        }
    }
}

fn invalidate_http_server_caches(message: &Value, cache_key: &str) {
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return;
    };
    if method == "notifications/tools/list_changed" {
        remove_cached_tool_descriptors(cache_key);
    }
    if method == "notifications/resources/list_changed"
        && let Ok(mut cache) = resource_cache().lock()
    {
        cache.remove(cache_key);
    }
    if method == "notifications/prompts/list_changed"
        && let Ok(mut cache) = prompt_cache().lock()
    {
        cache.remove(cache_key);
    }
}

#[cfg(test)]
fn handle_http_server_message(message: &Value, cache_key: &str) {
    invalidate_http_server_caches(message, cache_key);
}

fn parse_sse_events_from_buffer(buffer: &mut String) -> Vec<String> {
    let mut events = Vec::new();
    loop {
        let normalized = buffer.replace("\r\n", "\n");
        let Some(index) = normalized.find("\n\n") else {
            if normalized.len() != buffer.len() {
                *buffer = normalized;
            }
            break;
        };
        let event = normalized[..index].to_string();
        *buffer = normalized[index + 2..].to_string();
        if !event.trim().is_empty() {
            events.push(event);
        }
    }
    events
}

fn parse_sse_event_message(event: &str) -> Option<Value> {
    let mut data_lines = Vec::new();
    for line in event.lines() {
        if line.strip_prefix("id:").is_some() {
            continue;
        }
        if let Some(data) = line.strip_prefix("data:") {
            let trimmed = data.trim();
            if trimmed == "[DONE]" {
                return None;
            }
            data_lines.push(trimmed.to_string());
        }
    }
    if data_lines.is_empty() {
        return None;
    }
    let payload = data_lines.join("\n");
    serde_json::from_str::<Value>(&payload).ok()
}

fn record_sse_event_id(event: &str, cache_key: &str) {
    for line in event.lines() {
        if let Some(event_id) = line.strip_prefix("id:") {
            let event_id = event_id.trim();
            if !event_id.is_empty() {
                set_http_last_event_id(cache_key, event_id);
            }
        }
    }
}

#[cfg(test)]
fn parse_sse_event(event: &str, cache_key: &str) -> Option<Value> {
    record_sse_event_id(event, cache_key);
    let message = parse_sse_event_message(event)?;
    if message.get("method").is_some() {
        handle_http_server_message(&message, cache_key);
    }
    Some(message)
}

fn http_server_request_response(
    message: &Value,
    server_name: &str,
    workspace_root: &Path,
    client_capabilities: &McpClientCapabilityPolicy,
) -> Option<Value> {
    let method = message.get("method").and_then(Value::as_str)?;
    let id = message.get("id")?;
    let response = match method {
        "ping" => json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "result": {}
        }),
        "roots/list" if client_capabilities.roots => json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "result": workspace_roots_result(server_name, workspace_root)
        }),
        "sampling/createMessage" if client_capabilities.sampling => json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "error": {
                "code": -32000,
                "message": "MCP sampling is not enabled for this LingClaw session"
            }
        }),
        "elicitation/create" if client_capabilities.elicitation => json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "error": {
                "code": -32000,
                "message": "MCP elicitation is not enabled for this LingClaw session"
            }
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
    Some(response)
}

async fn send_http_jsonrpc_message(
    server_name: &str,
    server: &JsonMcpServerConfig,
    session_id: Option<&str>,
    payload: Value,
    timeout_secs: u64,
) -> Result<(), String> {
    let url = server
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("MCP server '{server_name}' is missing streamable-http url"))?;
    let client = reqwest_client_with_timeout(timeout_secs)?;
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
        .json(&payload);
    if let Some(session_id) = session_id {
        request = request.header("mcp-session-id", session_id);
    }
    if let Some(token) = bearer_token_for_server_config(server_name, server, timeout_secs).await? {
        request = request.bearer_auth(token);
    }
    for (key, value) in &server.headers {
        request = request.header(key, resolve_env_placeholder(value));
    }
    let response =
        send_http_request_with_timeout(request, timeout_secs, "HTTP MCP client response").await?;
    if !response.status().is_success() {
        return Err(format!(
            "HTTP MCP client response failed with {}",
            response.status()
        ));
    }
    Ok(())
}

async fn handle_http_server_message_async(
    message: &Value,
    cache_key: &str,
    server_name: &str,
    server: &JsonMcpServerConfig,
    workspace_root: &Path,
    session_id: Option<&str>,
    timeout_secs: u64,
) {
    invalidate_http_server_caches(message, cache_key);
    let Some(response) = http_server_request_response(
        message,
        server_name,
        workspace_root,
        &client_capabilities_for_server(server_name, workspace_root),
    ) else {
        return;
    };
    let _ =
        send_http_jsonrpc_message(server_name, server, session_id, response, timeout_secs).await;
}

async fn start_http_event_stream(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    session_id: &str,
    workspace_root: &Path,
    timeout_secs: u64,
) {
    let mut tasks = match http_stream_tasks().lock() {
        Ok(tasks) => tasks,
        Err(_) => return,
    };
    if tasks.contains_key(cache_key) {
        return;
    }

    let server_name = server_name.to_string();
    let server = server.clone();
    let cache_key = cache_key.to_string();
    let task_cache_key = cache_key.clone();
    let session_id = session_id.to_string();
    let workspace_root = workspace_root.to_path_buf();
    let task_id = next_http_stream_task_id();
    let handle = tokio::spawn(async move {
        tokio::task::yield_now().await;
        let _cleanup = HttpStreamTaskCleanup {
            cache_key: task_cache_key.clone(),
            task_id,
        };
        let Some(url) = server.url.as_deref() else {
            return;
        };
        let Ok(client) = reqwest_client_with_timeout(timeout_secs) else {
            return;
        };
        let mut request = client
            .get(url)
            .header("accept", "text/event-stream")
            .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
            .header("mcp-session-id", &session_id);
        if let Some(last_event_id) = http_last_event_id(&task_cache_key) {
            request = request.header("last-event-id", last_event_id);
        }
        if let Ok(Some(token)) =
            bearer_token_for_server_config(&server_name, &server, timeout_secs).await
        {
            request = request.bearer_auth(token);
        }
        for (key, value) in &server.headers {
            request = request.header(key, resolve_env_placeholder(value));
        }

        let response = match send_http_request_with_timeout(
            request,
            timeout_secs,
            "HTTP MCP event stream connect",
        )
        .await
        {
            Ok(response) => response,
            Err(_) => {
                return;
            }
        };
        if response.status() == HttpStatusCode::NOT_FOUND {
            remove_http_session_state(&task_cache_key);
            return;
        }
        if !response.status().is_success() {
            return;
        }
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        while let Some(chunk) = stream.next().await {
            let Ok(chunk) = chunk else {
                break;
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            for event in parse_sse_events_from_buffer(&mut buffer) {
                record_sse_event_id(&event, &task_cache_key);
                if let Some(message) = parse_sse_event_message(&event) {
                    handle_http_server_message_async(
                        &message,
                        &task_cache_key,
                        &server_name,
                        &server,
                        &workspace_root,
                        Some(&session_id),
                        timeout_secs,
                    )
                    .await;
                }
            }
        }
        if !buffer.trim().is_empty() {
            record_sse_event_id(&buffer, &task_cache_key);
            if let Some(message) = parse_sse_event_message(&buffer) {
                handle_http_server_message_async(
                    &message,
                    &task_cache_key,
                    &server_name,
                    &server,
                    &workspace_root,
                    Some(&session_id),
                    timeout_secs,
                )
                .await;
            }
        }
    });
    tasks.insert(cache_key, HttpStreamTaskEntry { task_id, handle });
}

async fn send_http_cancelled_notification(
    server_name: &str,
    server: &JsonMcpServerConfig,
    session_id: Option<&str>,
    request_id: Value,
    reason: &str,
) {
    let Some(url) = server.url.as_deref() else {
        return;
    };
    let Ok(client) = reqwest_client_with_timeout(2) else {
        return;
    };
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
        .json(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {
                "requestId": request_id,
                "reason": reason,
            }
        }));
    if let Some(session_id) = session_id {
        request = request.header("mcp-session-id", session_id);
    }
    if let Ok(Some(token)) = bearer_token_for_server_config(server_name, server, 2).await {
        request = request.bearer_auth(token);
    }
    for (key, value) in &server.headers {
        request = request.header(key, resolve_env_placeholder(value));
    }
    let _ = send_http_request_with_timeout(request, 2, "HTTP MCP cancellation").await;
}

async fn http_post_json(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    workspace_root: &Path,
    payload: Value,
    session_id: Option<String>,
    timeout_secs: u64,
) -> Result<Value, String> {
    let url = server
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("MCP server '{server_name}' is missing streamable-http url"))?;
    let client = reqwest_client_with_timeout(timeout_secs)?;
    let mut request = client
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
        .json(&payload);
    if let Some(session_id) = session_id.as_deref() {
        request = request.header("mcp-session-id", session_id);
    }
    if let Some(token) = bearer_token_for_server_config(server_name, server, timeout_secs).await? {
        request = request.bearer_auth(token);
    }
    for (key, value) in &server.headers {
        request = request.header(key, resolve_env_placeholder(value));
    }

    let request_id = payload.get("id").cloned();
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("request")
        .to_string();
    let response =
        match send_http_request_with_timeout(request, timeout_secs, "HTTP MCP request").await {
            Ok(response) => response,
            Err(error) => {
                if error.contains("timed out after") {
                    if let Some(request_id) = request_id.clone() {
                        send_http_cancelled_notification(
                            server_name,
                            server,
                            session_id.as_deref(),
                            request_id,
                            &format!("{method} timed out"),
                        )
                        .await;
                    }
                    remove_http_session(cache_key);
                }
                return Err(error);
            }
        };
    if response.status() == HttpStatusCode::NOT_FOUND {
        remove_http_session(cache_key);
        return Err("HTTP MCP session not found".to_string());
    }
    if response.status() == HttpStatusCode::UNAUTHORIZED {
        return Err(format!(
            "HTTP MCP server '{server_name}' requires authorization"
        ));
    }
    if !response.status().is_success() {
        let status = response.status();
        let text = response_text_with_timeout(
            response,
            timeout_secs,
            "failed to read HTTP MCP error response",
        )
        .await
        .unwrap_or_default();
        return Err(format!("HTTP MCP request failed with {status}: {text}"));
    }

    let next_session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if next_session_id.is_some() {
        set_http_session_id(cache_key, next_session_id.clone());
    }
    let effective_session_id = next_session_id.as_deref().or(session_id.as_deref());
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if request_id.is_none() && method.starts_with("notifications/") {
        return Ok(json!({}));
    }
    if content_type.contains("text/event-stream") {
        return parse_sse_json_response_stream(
            response,
            cache_key,
            request_id,
            server_name,
            server,
            workspace_root,
            effective_session_id,
            timeout_secs,
        )
        .await;
    }
    let text = match response_text_with_timeout(
        response,
        timeout_secs,
        "failed to read HTTP MCP response",
    )
    .await
    {
        Ok(text) => text,
        Err(error) => {
            if error.contains("timed out after") {
                if let Some(request_id) = request_id.clone() {
                    send_http_cancelled_notification(
                        server_name,
                        server,
                        session_id.as_deref(),
                        request_id,
                        &format!("{method} timed out"),
                    )
                    .await;
                }
                remove_http_session(cache_key);
            }
            return Err(error);
        }
    };
    serde_json::from_str(&text).map_err(|error| format!("invalid HTTP MCP JSON: {error}"))
}

#[cfg(test)]
fn parse_sse_json_response(text: &str, cache_key: &str) -> Result<Value, String> {
    let mut buffer = text.to_string();
    let mut last_response = None;
    for event in parse_sse_events_from_buffer(&mut buffer) {
        let Some(message) = parse_sse_event(&event, cache_key) else {
            continue;
        };
        if message.get("id").is_some()
            || message.get("result").is_some()
            || message.get("error").is_some()
        {
            last_response = Some(message);
        }
    }
    if !buffer.trim().is_empty()
        && let Some(message) = parse_sse_event(&buffer, cache_key)
        && (message.get("id").is_some()
            || message.get("result").is_some()
            || message.get("error").is_some())
    {
        last_response = Some(message);
    }
    last_response.ok_or_else(|| "HTTP MCP SSE response did not contain data".to_string())
}

async fn parse_sse_json_response_stream(
    response: reqwest::Response,
    cache_key: &str,
    expected_id: Option<Value>,
    server_name: &str,
    server: &JsonMcpServerConfig,
    workspace_root: &Path,
    session_id: Option<&str>,
    timeout_secs: u64,
) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut last_response = None;

    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            let error = format!("HTTP MCP SSE response timed out after {timeout_secs}s");
            if let Some(request_id) = expected_id.clone() {
                send_http_cancelled_notification(
                    server_name,
                    server,
                    session_id,
                    request_id,
                    &error,
                )
                .await;
            }
            remove_http_session(cache_key);
            return Err(error);
        };
        let chunk = match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(error))) => {
                return Err(format!("failed to read HTTP MCP SSE response: {error}"));
            }
            Ok(None) => break,
            Err(_) => {
                let error = format!("HTTP MCP SSE response timed out after {timeout_secs}s");
                if let Some(request_id) = expected_id.clone() {
                    send_http_cancelled_notification(
                        server_name,
                        server,
                        session_id,
                        request_id,
                        &error,
                    )
                    .await;
                }
                remove_http_session(cache_key);
                return Err(error);
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));
        for event in parse_sse_events_from_buffer(&mut buffer) {
            record_sse_event_id(&event, cache_key);
            let Some(message) = parse_sse_event_message(&event) else {
                continue;
            };
            handle_http_server_message_async(
                &message,
                cache_key,
                server_name,
                server,
                workspace_root,
                session_id,
                timeout_secs,
            )
            .await;

            if let Some(expected_id) = expected_id.as_ref() {
                if message.get("id") == Some(expected_id)
                    && (message.get("result").is_some() || message.get("error").is_some())
                {
                    return Ok(message);
                }
            } else if message.get("result").is_some() || message.get("error").is_some() {
                last_response = Some(message);
            }
        }
    }

    if !buffer.trim().is_empty() {
        record_sse_event_id(&buffer, cache_key);
        if let Some(message) = parse_sse_event_message(&buffer) {
            handle_http_server_message_async(
                &message,
                cache_key,
                server_name,
                server,
                workspace_root,
                session_id,
                timeout_secs,
            )
            .await;
            if let Some(expected_id) = expected_id.as_ref() {
                if message.get("id") == Some(expected_id)
                    && (message.get("result").is_some() || message.get("error").is_some())
                {
                    return Ok(message);
                }
            } else if message.get("result").is_some() || message.get("error").is_some() {
                last_response = Some(message);
            }
        }
    }

    last_response.ok_or_else(|| "HTTP MCP SSE response did not contain data".to_string())
}

async fn initialize_http_session(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    workspace: &Path,
    start_event_stream: bool,
    timeout_secs: u64,
) -> Result<Option<String>, String> {
    let capabilities =
        initialize_capabilities(&client_capabilities_for_server(server_name, workspace));
    let init = http_post_json(
        server_name,
        server,
        cache_key,
        workspace,
        json!({
            "jsonrpc": "2.0",
            "id": next_http_request_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": capabilities,
                "clientInfo": {
                    "name": "LingClaw",
                    "version": VERSION
                }
            }
        }),
        None,
        timeout_secs,
    )
    .await?;
    if let Some(error) = init.get("error") {
        return Err(format!(
            "initialize failed: {}",
            serde_json::to_string(error).unwrap_or_else(|_| error.to_string())
        ));
    }
    let session_id = http_session_id(cache_key);
    let _ = http_post_json(
        server_name,
        server,
        cache_key,
        workspace,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
        session_id.clone(),
        timeout_secs,
    )
    .await;
    if start_event_stream && let Some(session_id) = session_id.as_deref() {
        start_http_event_stream(
            server_name,
            server,
            cache_key,
            session_id,
            workspace,
            timeout_secs,
        )
        .await;
    }
    Ok(session_id)
}

async fn call_http_server(
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
    let timeout_secs = server_timeout_secs(server, config);
    let cache_key = cache_key(server_name, server, workspace, config)?;
    let mut session_id = http_session_id(&cache_key);
    let session_was_cached;
    if session_id.is_none() {
        let init_lock = http_initialization_lock(&cache_key);
        let _init_guard = init_lock.lock().await;
        session_id = http_session_id(&cache_key);
        if session_id.is_none() {
            session_id = initialize_http_session(
                server_name,
                server,
                &cache_key,
                workspace,
                true,
                timeout_secs,
            )
            .await?;
            session_was_cached = false;
        } else {
            session_was_cached = true;
        }
    } else {
        session_was_cached = true;
    }
    if session_was_cached && let Some(session_id) = session_id.as_deref() {
        start_http_event_stream(
            server_name,
            server,
            &cache_key,
            session_id,
            workspace,
            timeout_secs,
        )
        .await;
    }
    let payload = json!({
        "jsonrpc": "2.0",
        "id": next_http_request_id(),
        "method": method,
        "params": params,
    });
    let response = match http_post_json(
        server_name,
        server,
        &cache_key,
        workspace,
        payload.clone(),
        session_id.clone(),
        timeout_secs,
    )
    .await
    {
        Err(error) if error == "HTTP MCP session not found" => {
            let init_lock = http_initialization_lock(&cache_key);
            let _init_guard = init_lock.lock().await;
            session_id = http_session_id(&cache_key);
            if session_id.is_none() {
                session_id = initialize_http_session(
                    server_name,
                    server,
                    &cache_key,
                    workspace,
                    true,
                    timeout_secs,
                )
                .await?;
            }
            http_post_json(
                server_name,
                server,
                &cache_key,
                workspace,
                payload,
                session_id,
                timeout_secs,
            )
            .await?
        }
        other => other?,
    };
    if let Some(error) = response.get("error") {
        return Err(serde_json::to_string(error).unwrap_or_else(|_| error.to_string()));
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| format!("server response missing result for method '{method}'"))
}

async fn terminate_http_session(server_name: &str, cache_key: &str, server: &JsonMcpServerConfig) {
    let Some(session_id) = http_session_id(cache_key) else {
        return;
    };
    let Some(url) = server.url.as_deref() else {
        return;
    };
    let Ok(client) = reqwest_client_with_timeout(2) else {
        remove_http_session(cache_key);
        return;
    };
    let mut request = client
        .delete(url)
        .header("mcp-session-id", session_id)
        .header("mcp-protocol-version", MCP_PROTOCOL_VERSION);
    if let Ok(Some(token)) = bearer_token_for_server_config(server_name, server, 2).await {
        request = request.bearer_auth(token);
    }
    for (key, value) in &server.headers {
        request = request.header(key, resolve_env_placeholder(value));
    }
    let _ = send_http_request_with_timeout(request, 2, "HTTP MCP session terminate").await;
    remove_http_session(cache_key);
}

impl TemporaryMcpSession {
    async fn new(server_name: &str, config: &Config, workspace: &Path) -> Result<Self, String> {
        let server = config
            .mcp_servers
            .get(server_name)
            .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
        if !server.enabled {
            return Err(format!("MCP server '{server_name}' is disabled"));
        }
        if is_streamable_http_server(server) {
            let timeout_secs = server_timeout_secs(server, config);
            let base_key = cache_key(server_name, server, workspace, config)?;
            let cache_key = format!("{base_key}\none-shot\n{:?}", Instant::now());
            let session_id = initialize_http_session(
                server_name,
                server,
                &cache_key,
                workspace,
                false,
                timeout_secs,
            )
            .await?;
            return Ok(Self::Http {
                server_name: server_name.to_string(),
                server: server.clone(),
                cache_key,
                session_id,
                timeout_secs,
            });
        }

        spawn_server_session(server_name, config, workspace)
            .await
            .map(Self::Stdio)
    }

    async fn request(
        &mut self,
        workspace: &Path,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match self {
            Self::Http {
                server_name,
                server,
                cache_key,
                session_id,
                timeout_secs,
            } => {
                let payload = json!({
                    "jsonrpc": "2.0",
                    "id": next_http_request_id(),
                    "method": method,
                    "params": params,
                });
                let response = http_post_json(
                    server_name,
                    server,
                    cache_key,
                    workspace,
                    payload,
                    session_id.clone(),
                    *timeout_secs,
                )
                .await?;
                *session_id = http_session_id(cache_key);
                if let Some(error) = response.get("error") {
                    return Err(serde_json::to_string(error).unwrap_or_else(|_| error.to_string()));
                }
                response
                    .get("result")
                    .cloned()
                    .ok_or_else(|| format!("server response missing result for method '{method}'"))
            }
            Self::Stdio(session) => match session.request(method, params).await {
                Ok(result) => Ok(result),
                Err(error) => Err(session.decorate_error(error)),
            },
        }
    }

    async fn shutdown(&mut self) {
        match self {
            Self::Http {
                server_name,
                server,
                cache_key,
                ..
            } => {
                terminate_http_session(server_name, cache_key, server).await;
            }
            Self::Stdio(session) => {
                session.shutdown().await;
            }
        }
    }
}

async fn call_server_once(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let mut session = TemporaryMcpSession::new(server_name, config, workspace).await?;
    let result = session.request(workspace, method, params).await;
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
    if is_streamable_http_server(server) {
        return call_http_server(server_name, config, workspace, method, params).await;
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
    client_capabilities: &McpClientCapabilityPolicy,
) -> Result<Value, String>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let message = read_message(reader, stdout_lines).await?;
        if message.get("id").and_then(Value::as_u64) == Some(expected_id)
            && message.get("method").is_none()
            && (message.get("result").is_some() || message.get("error").is_some())
        {
            return Ok(message);
        }
        handle_server_message(
            stdin,
            &message,
            stdout_lines,
            server_name,
            workspace_root,
            tool_cache_key,
            client_capabilities,
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
    client_capabilities: &McpClientCapabilityPolicy,
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
        if method == "notifications/resources/list_changed"
            && let Ok(mut cache) = resource_cache().lock()
        {
            cache.remove(tool_cache_key);
        }
        if method == "notifications/prompts/list_changed"
            && let Ok(mut cache) = prompt_cache().lock()
        {
            cache.remove(tool_cache_key);
        }

        if let Some(id) = message.get("id") {
            let response = match method {
                "ping" => json!({
                    "jsonrpc": "2.0",
                    "id": id.clone(),
                    "result": {}
                }),
                "roots/list" if client_capabilities.roots => json!({
                    "jsonrpc": "2.0",
                    "id": id.clone(),
                    "result": workspace_roots_result(server_name, workspace_root)
                }),
                "sampling/createMessage" if client_capabilities.sampling => json!({
                    "jsonrpc": "2.0",
                    "id": id.clone(),
                    "error": {
                        "code": -32000,
                        "message": "MCP sampling is not enabled for this LingClaw session"
                    }
                }),
                "elicitation/create" if client_capabilities.elicitation => json!({
                    "jsonrpc": "2.0",
                    "id": id.clone(),
                    "error": {
                        "code": -32000,
                        "message": "MCP elicitation is not enabled for this LingClaw session"
                    }
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
