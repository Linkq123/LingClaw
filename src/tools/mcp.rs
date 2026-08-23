use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use futures::{StreamExt, future::join_all};
use reqwest::StatusCode as HttpStatusCode;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::{Mutex as AsyncMutex, Notify, watch},
    task::JoinHandle,
};

use crate::tools::safety::{
    CheckedWorkspaceChildRoot, CheckedWorkspaceCwd, CheckedWorkspacePath, resolve_path_checked,
};
use crate::{Config, VERSION, config::JsonMcpServerConfig, config_dir_path};

use super::{ToolImageOutput, ToolOutcome};

const MCP_NAME_PREFIX: &str = "mcp__";
const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const MCP_DIAGNOSTIC_LINE_LIMIT: usize = 6;
const MCP_DIAGNOSTIC_CHAR_LIMIT: usize = 400;
const MCP_STDIO_TRANSPORT_ERROR_PREFIX: &str = "MCP stdio transport error";
const MCP_TOOL_CACHE_TTL_SECS: u64 = 30;
const MCP_SESSION_IDLE_TTL_SECS: u64 = 300;
const MCP_SPAWN_FAILURE_COOLDOWN_SECS: u64 = 15;
#[cfg(test)]
const MCP_DEFAULT_HTTP_TIMEOUT_SECS: u64 = 30;
const MCP_MAX_PAGINATION_PAGES: usize = 100;
const MCP_SESSION_POLICY_FILE: &str = ".lingclaw-mcp-policy.json";
const MCP_AUTH_FILE: &str = "mcp-auth.json";
static MCP_TOOL_CACHE: OnceLock<Mutex<HashMap<String, CachedToolDescriptors>>> = OnceLock::new();

/// Shared image budget for one Agent tool-call batch. Each parallel call gets
/// an ordered ticket so earlier tool calls get first use of the batch budget,
/// regardless of network completion order.
#[derive(Clone, Debug)]
pub(crate) struct ToolImageBudget {
    inner: Arc<ToolImageBudgetInner>,
    call_index: Option<usize>,
    _completion: Option<Arc<ToolImageCallCompletion>>,
    wait_state: Option<Arc<ToolImageWaitState>>,
}

#[derive(Debug)]
struct ToolImageBudgetInner {
    remaining: AtomicUsize,
    order: Mutex<ToolImageBudgetOrder>,
    order_changed: Notify,
}

#[derive(Debug, Default)]
struct ToolImageBudgetOrder {
    next_call: usize,
    completed_calls: BTreeSet<usize>,
}

#[derive(Debug)]
struct ToolImageCallCompletion {
    inner: Arc<ToolImageBudgetInner>,
    call_index: usize,
    completed: AtomicBool,
}

#[derive(Debug)]
struct ToolImageWaitState {
    waiting: watch::Sender<bool>,
}

struct ToolImageWaitGuard<'a> {
    state: &'a ToolImageWaitState,
}

impl Drop for ToolImageWaitGuard<'_> {
    fn drop(&mut self) {
        self.state.waiting.send_replace(false);
    }
}

impl Drop for ToolImageCallCompletion {
    fn drop(&mut self) {
        if self.completed.swap(true, Ordering::AcqRel) {
            return;
        }

        let mut order = self
            .inner
            .order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        order.completed_calls.insert(self.call_index);
        loop {
            let next_call = order.next_call;
            if !order.completed_calls.remove(&next_call) {
                break;
            }
            order.next_call = order.next_call.saturating_add(1);
        }
        drop(order);
        self.inner.order_changed.notify_waiters();
    }
}

impl ToolImageBudget {
    pub(crate) fn new(max_images: usize) -> Self {
        Self {
            inner: Arc::new(ToolImageBudgetInner {
                remaining: AtomicUsize::new(max_images),
                order: Mutex::new(ToolImageBudgetOrder::default()),
                order_changed: Notify::new(),
            }),
            call_index: None,
            _completion: None,
            wait_state: None,
        }
    }

    /// Create a budget ticket for one tool call in the provider's original
    /// call order. The ticket marks the call complete when its last clone is
    /// dropped, including cancellation and timeout paths.
    pub(crate) fn for_call(&self, call_index: usize) -> Self {
        let (waiting, _) = watch::channel(false);
        Self {
            inner: Arc::clone(&self.inner),
            call_index: Some(call_index),
            _completion: Some(Arc::new(ToolImageCallCompletion {
                inner: Arc::clone(&self.inner),
                call_index,
                completed: AtomicBool::new(false),
            })),
            wait_state: Some(Arc::new(ToolImageWaitState { waiting })),
        }
    }

    /// Subscribe to time spent waiting for earlier tool calls. Parallel tool
    /// runners use this signal to pause only their individual runtime timeout;
    /// cancellation and the enclosing Agent/Sub-agent deadline remain active.
    pub(crate) fn subscribe_waiting(&self) -> Option<watch::Receiver<bool>> {
        self.wait_state
            .as_ref()
            .map(|state| state.waiting.subscribe())
    }

    /// Wait until every earlier tool call has completed. The root budget used
    /// by compatibility wrappers has no call index and therefore never waits.
    pub(crate) async fn wait_for_turn(&self) {
        let Some(call_index) = self.call_index else {
            return;
        };

        let mut wait_guard = None;

        loop {
            // Register before checking the condition to avoid losing a wakeup
            // between the mutex check and awaiting the notification.
            let changed = self.inner.order_changed.notified();
            let ready = self
                .inner
                .order
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .next_call
                >= call_index;
            if ready {
                return;
            }
            if wait_guard.is_none()
                && let Some(state) = self.wait_state.as_deref()
            {
                state.waiting.send_replace(true);
                wait_guard = Some(ToolImageWaitGuard { state });
            }
            changed.await;
        }
    }

    pub(crate) fn try_reserve(&self) -> bool {
        self.inner
            .remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
    }

    pub(crate) fn release(&self) {
        self.inner.remaining.fetch_add(1, Ordering::Release);
    }
}
static MCP_RESOURCE_CACHE: OnceLock<Mutex<HashMap<String, CachedResourceDescriptors>>> =
    OnceLock::new();
static MCP_PROMPT_CACHE: OnceLock<Mutex<HashMap<String, CachedPromptDescriptors>>> =
    OnceLock::new();
static MCP_SESSION_CACHE: OnceLock<Mutex<HashMap<String, CachedMcpSession>>> = OnceLock::new();
static MCP_HTTP_RUNTIME_STATE: OnceLock<Mutex<HttpRuntimeState>> = OnceLock::new();
static MCP_SPAWN_FAILURES: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
static MCP_NEXT_HTTP_STREAM_TASK_ID: AtomicU64 = AtomicU64::new(1);
static MCP_NEXT_HTTP_CLEANUP_ID: AtomicU64 = AtomicU64::new(1);
static MCP_NEXT_HTTP_SESSION_GENERATION: AtomicU64 = AtomicU64::new(1);
static MCP_NEXT_HTTP_SESSION_EPOCH: AtomicU64 = AtomicU64::new(1);
static MCP_NEXT_HTTP_REQUEST_ID: AtomicU64 = AtomicU64::new(2);
#[cfg(test)]
static MCP_AUTH_FILE_OVERRIDE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
#[cfg(test)]
static MCP_TEST_GUARD: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
#[cfg(test)]
static MCP_HTTP_EXCLUSIVE_WAIT_SIGNALS: OnceLock<
    Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>,
> = OnceLock::new();
#[cfg(test)]
static MCP_HTTP_DESCRIPTOR_INSERT_BARRIER: OnceLock<
    Mutex<Option<HttpDescriptorInsertTestBarrier>>,
> = OnceLock::new();
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
    pub(crate) annotations: McpToolAnnotations,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct McpToolAnnotations {
    pub(crate) read_only_hint: Option<bool>,
    pub(crate) destructive_hint: Option<bool>,
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
    /// Runtime-only namespace for descriptor caches and persistent MCP
    /// connections. Session policy files live in the private Session Home, so
    /// this keeps two Sessions that share one project directory isolated.
    #[serde(skip)]
    pub(crate) cache_namespace: Option<PathBuf>,
}

impl McpSessionPolicy {
    pub(crate) fn allows_server(&self, server_name: &str) -> bool {
        self.enabled_servers.contains(server_name)
    }

    pub(crate) fn allows_tool(&self, descriptor: &McpToolDescriptor) -> bool {
        self.allows_server(&descriptor.server_name)
            && self.enabled_tools.contains(&descriptor.exposed_name)
    }

    fn cache_namespace<'a>(&'a self, fallback: &'a Path) -> &'a Path {
        self.cache_namespace.as_deref().unwrap_or(fallback)
    }

    fn client_capabilities_for_server(&self, server_name: &str) -> McpClientCapabilityPolicy {
        if self.allows_server(server_name) {
            effective_client_capabilities(&self.client_capabilities)
        } else {
            McpClientCapabilityPolicy::default()
        }
    }
}

#[derive(Clone, Debug)]
struct CachedToolDescriptors {
    descriptors: Vec<McpToolDescriptor>,
    loaded_at: Instant,
    http_authority: Option<HttpDescriptorCacheAuthority>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct CachedResourceDescriptors {
    descriptors: Vec<McpResourceDescriptor>,
    loaded_at: Instant,
    http_authority: Option<HttpDescriptorCacheAuthority>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct CachedPromptDescriptors {
    descriptors: Vec<McpPromptDescriptor>,
    loaded_at: Instant,
    http_authority: Option<HttpDescriptorCacheAuthority>,
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
    epoch: u64,
    generation: u64,
    last_used_at: Instant,
    workspace_root: CheckedWorkspacePath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HttpSessionIdentity {
    session_id: String,
    epoch: u64,
    generation: u64,
}

#[derive(Clone, Debug)]
struct HttpSessionSupersession {
    remote_domain_key: String,
    replacement_cache_key: String,
    replacement: HttpSessionIdentity,
}

enum HttpSessionLookup {
    Missing,
    Active(Option<String>),
    Expired(HttpSessionIdentity),
}

struct HttpRequestAuthority {
    // Drop the generation-bound request count before the control lease so the
    // latter can reclaim an otherwise empty control on cancellation paths.
    _in_flight: Option<HttpInFlightRequestLease>,
    control: HttpControlLease,
    epoch: u64,
    identity: Option<HttpSessionIdentity>,
    descriptor_epoch: u64,
    initialize: bool,
}

impl HttpRequestAuthority {
    fn release_in_flight(&mut self) {
        // Once this response has reached a terminal local validation path, its
        // own remote request can no longer race a cleanup DELETE. Release only
        // this generation-bound lease before cleanup; any concurrent request
        // for the same generation remains counted and still defers DELETE.
        if let Some(mut in_flight) = self._in_flight.take() {
            in_flight.completed = true;
            drop(in_flight);
        }
    }

    fn abandon_in_flight(&mut self) {
        // Dropping an unfinished request while a cleanup is waiting means the
        // remote POST may still have a late side effect. The lease Drop marks
        // that exact deferred cleanup uncertain before waking its worker.
        self._in_flight.take();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct HttpDescriptorCacheAuthority {
    cache_key: String,
    epoch: u64,
    identity: Option<HttpSessionIdentity>,
    descriptor_epoch: u64,
}

struct HttpDescriptorCachePermit {
    authority: HttpDescriptorCacheAuthority,
    _control: HttpControlLease,
}

#[cfg(test)]
struct HttpDescriptorInsertTestBarrier {
    cache_key: String,
    cache_kind: &'static str,
    reached: Option<tokio::sync::oneshot::Sender<()>>,
    release: tokio::sync::oneshot::Receiver<()>,
}

struct HttpPostJsonResult {
    value: Value,
    descriptor_authority: HttpDescriptorCacheAuthority,
}

struct HttpCallResult {
    value: Value,
    descriptor_authority: Option<HttpDescriptorCacheAuthority>,
}

struct ListedServerItems {
    items: Vec<Value>,
    descriptor_authority: Option<HttpDescriptorCacheAuthority>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct HttpInFlightRequestKey {
    cache_key: String,
    epoch: u64,
    generation: u64,
}

struct HttpInFlightRequestLease {
    key: HttpInFlightRequestKey,
    completed: bool,
}

struct HttpDeferredCleanupState {
    cleanup_id: u64,
    notify: Arc<Notify>,
    remote_cleanup: Option<(String, u64)>,
    force_uncertain: bool,
}

struct HttpDeferredCleanupWait {
    key: HttpInFlightRequestKey,
    cleanup_id: u64,
    notify: Arc<Notify>,
}

struct CachedHttpEventId {
    epoch: u64,
    generation: u64,
    event_id: String,
}

struct HttpStreamTaskEntry {
    task_id: u64,
    epoch: u64,
    generation: u64,
    handle: JoinHandle<()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HttpDeleteOutcome {
    Confirmed,
    NotApplied,
    Ambiguous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HttpCleanupPhase {
    Pending,
    Uncertain,
}

struct HttpCleanupState {
    cleanup_id: u64,
    phase: HttpCleanupPhase,
    sticky_uncertainty: bool,
    owner_cache_key: Option<String>,
    owner_identity: Option<HttpSessionIdentity>,
}

struct HttpCleanupTaskEntry {
    cleanup_id: u64,
    _handle: JoinHandle<()>,
}

struct HttpSessionControl {
    epoch: AtomicU64,
    descriptor_epoch: AtomicU64,
    leases: AtomicUsize,
    exclusive: Arc<AsyncMutex<()>>,
}

impl HttpSessionControl {
    fn new() -> Self {
        Self {
            epoch: AtomicU64::new(next_http_session_epoch()),
            descriptor_epoch: AtomicU64::new(1),
            leases: AtomicUsize::new(0),
            exclusive: Arc::new(AsyncMutex::new(())),
        }
    }
}

#[derive(Default)]
struct HttpRuntimeState {
    controls: HashMap<String, Arc<HttpSessionControl>>,
    sessions: HashMap<String, CachedHttpMcpSession>,
    last_event_ids: HashMap<String, CachedHttpEventId>,
    stream_tasks: HashMap<String, HttpStreamTaskEntry>,
    cleanups: HashMap<String, HttpCleanupState>,
    cleanup_tasks: HashMap<String, HttpCleanupTaskEntry>,
    remote_domains_by_server: HashMap<String, HashSet<String>>,
    remote_domain_by_cache_key: HashMap<String, String>,
    supersessions: HashMap<String, HttpSessionSupersession>,
    in_flight_requests: HashMap<HttpInFlightRequestKey, usize>,
    deferred_cleanups: HashMap<HttpInFlightRequestKey, HttpDeferredCleanupState>,
}

struct HttpControlLease {
    cache_key: String,
    control: Arc<HttpSessionControl>,
}

impl Clone for HttpControlLease {
    fn clone(&self) -> Self {
        self.control.leases.fetch_add(1, Ordering::AcqRel);
        Self {
            cache_key: self.cache_key.clone(),
            control: self.control.clone(),
        }
    }
}

struct HttpKeyExclusiveGuard {
    // Fields intentionally drop in this order: release the async mutex before
    // the lease can make the now-idle control eligible for reclamation.
    _guard: tokio::sync::OwnedMutexGuard<()>,
    lease: HttpControlLease,
}

struct HttpOneShotScopeAuthority {
    cache_key: String,
    control: HttpControlLease,
    epoch: u64,
}

struct HttpCleanupQuarantineObserver {
    cache_key: String,
    cleanup_id: u64,
    _control: HttpControlLease,
    completed: bool,
}

struct HttpOneShotInitializeAttempt {
    cache_key: String,
    _control: HttpControlLease,
    installed_session: Option<(String, HttpSessionIdentity)>,
    may_have_been_sent: bool,
    observed_cleanup: Option<HttpDeleteOutcome>,
    completed: bool,
}

struct TemporaryHttpLifecycleOwner {
    cache_key: String,
    remote_domain_key: String,
    _control: HttpControlLease,
    installed_session: Option<HttpSessionIdentity>,
    completed: bool,
}

struct TemporaryHttpMcpSession {
    server_name: String,
    server: JsonMcpServerConfig,
    cache_key: String,
    // Drop the lifecycle owner before releasing the endpoint guard so an
    // abandoned Session establishes its fail-closed tombstone while the
    // endpoint authority is still exclusively held.
    lifecycle: Option<TemporaryHttpLifecycleOwner>,
    one_shot_guard: Option<HttpKeyExclusiveGuard>,
    one_shot_scope: Option<HttpOneShotScopeAuthority>,
    session_id: Option<String>,
    workspace_root: CheckedWorkspacePath,
    client_capabilities: McpClientCapabilityPolicy,
    timeout_secs: u64,
}

impl Drop for TemporaryHttpMcpSession {
    fn drop(&mut self) {
        // Run the synchronous fail-closed owner while the endpoint-exclusive
        // guard is still a live field. Rust drops the remaining fields only
        // after this method returns.
        drop(self.lifecycle.take());
    }
}

enum TemporaryMcpSession {
    Http(Box<TemporaryHttpMcpSession>),
    Stdio(Box<McpServerSession>),
}

struct McpServerSession {
    server_name: String,
    workspace_root: CheckedWorkspacePath,
    workspace_child_root: CheckedWorkspaceChildRoot,
    server_cwd: CheckedWorkspacePath,
    process_cwd: CheckedWorkspaceCwd,
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

/// Trust the MCP server's explicit impact declaration for read-only execution.
/// Missing declarations and contradictory destructive declarations fail closed.
pub(crate) fn is_read_only_tool_descriptor(descriptor: &McpToolDescriptor) -> bool {
    descriptor.annotations.read_only_hint == Some(true)
        && descriptor.annotations.destructive_hint != Some(true)
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

pub(crate) fn is_read_only_tool_name_for_policy(
    name: &str,
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> bool {
    if !is_mcp_tool_name(name) {
        return false;
    }

    cached_list_tools_for_policy(config, workspace, policy)
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
    let mut policy: McpSessionPolicy = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    policy.cache_namespace = Some(workspace.to_path_buf());
    policy
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
    let builder =
        reqwest::Client::builder().connect_timeout(Duration::from_secs(timeout_secs.max(1)));
    #[cfg(test)]
    let builder = builder.no_proxy();

    builder
        .build()
        .map_err(|error| format!("failed to build HTTP client: {error}"))
}

fn streamable_http_client_with_timeout(timeout_secs: u64) -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(timeout_secs.max(1)))
        .redirect(reqwest::redirect::Policy::none());
    #[cfg(test)]
    let builder = builder.no_proxy();

    builder
        .build()
        .map_err(|error| format!("failed to build Streamable HTTP client: {error}"))
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

fn effective_http_client_capabilities(
    policy: &McpClientCapabilityPolicy,
) -> McpClientCapabilityPolicy {
    let mut effective = effective_client_capabilities(policy);
    // Streamable HTTP cannot transfer an OS directory capability to a remote
    // process. Advertising roots would turn a checked local root back into a
    // mutable file:// pathname between validation and network delivery.
    effective.roots = false;
    effective
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

    let candidates = vec![
        path_well_known(&server_url, "/.well-known/oauth-protected-resource")?,
        origin_well_known(&server_url, "/.well-known/oauth-protected-resource")?,
    ];
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

pub(crate) async fn ensure_tools_cached_for_policy(
    config: &Config,
    working_directory: &Path,
    policy: &McpSessionPolicy,
) {
    let _ = list_tools_for_policy(config, working_directory, policy).await;
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
        let cached = {
            let Ok(mut cache) = tool_cache().lock() else {
                continue;
            };
            match cache.get(&key) {
                Some(entry) if now.duration_since(entry.loaded_at) < tool_cache_ttl() => {
                    Some(entry.clone())
                }
                Some(_) => {
                    cache.remove(&key);
                    None
                }
                None => None,
            }
        };
        if let Some(entry) = cached {
            if cached_descriptor_authority_is_current(server, entry.http_authority.as_ref()) {
                cached_servers += 1;
            } else {
                remove_tool_cache_entry_if_unchanged(
                    &key,
                    entry.loaded_at,
                    entry.http_authority.as_ref(),
                );
            }
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
        let Ok(key) = cache_key_for_policy(server_name, server, workspace, config, policy) else {
            continue;
        };
        let required_tools = policy
            .enabled_tools
            .iter()
            .filter(|tool| exposed_tool_matches_server(tool, server_name))
            .map(String::as_str)
            .collect::<HashSet<_>>();
        let cached = {
            let Ok(mut cache) = tool_cache().lock() else {
                continue;
            };
            match cache.get(&key) {
                Some(entry) if now.duration_since(entry.loaded_at) < tool_cache_ttl() => {
                    Some(entry.clone())
                }
                Some(_) => {
                    cache.remove(&key);
                    None
                }
                None => None,
            }
        };
        let has_cache = cached.as_ref().is_some_and(|entry| {
            cached_descriptor_authority_is_current(server, entry.http_authority.as_ref()) && {
                let cached_tools = entry
                    .descriptors
                    .iter()
                    .map(|descriptor| descriptor.exposed_name.as_str())
                    .collect::<HashSet<_>>();
                required_tools
                    .iter()
                    .all(|tool| cached_tools.contains(tool))
            }
        });
        if has_cache {
            cached_servers += 1;
        } else if let Some(entry) = cached {
            remove_tool_cache_entry_if_unchanged(
                &key,
                entry.loaded_at,
                entry.http_authority.as_ref(),
            );
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
    execute_tool_with_session_mode(name, args_str, config, workspace, false, None, None).await
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
    execute_tool_with_session_mode(name, args_str, config, workspace, true, None, None).await
}

#[allow(dead_code)] // Compatibility wrapper for callers without a shared batch budget.
pub(crate) async fn execute_tool_for_policy(
    name: &str,
    args_str: &str,
    config: &Config,
    workspace: &Path,
    isolated_session: bool,
    policy: &McpSessionPolicy,
) -> Option<ToolOutcome> {
    execute_tool_for_policy_with_image_budget(
        name,
        args_str,
        config,
        workspace,
        isolated_session,
        policy,
        None,
    )
    .await
}

pub(crate) async fn execute_tool_for_policy_with_image_budget(
    name: &str,
    args_str: &str,
    config: &Config,
    workspace: &Path,
    isolated_session: bool,
    policy: &McpSessionPolicy,
    image_budget: Option<ToolImageBudget>,
) -> Option<ToolOutcome> {
    execute_tool_with_session_mode(
        name,
        args_str,
        config,
        workspace,
        isolated_session,
        Some(policy),
        image_budget,
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
    image_budget: Option<ToolImageBudget>,
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
                images: Vec::new(),
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
                images: Vec::new(),
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
                images: Vec::new(),
            });
        }
        Err(error) => {
            return Some(ToolOutcome {
                output: format!("{name} error: {error}"),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
                images: Vec::new(),
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
            images: Vec::new(),
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
            images: Vec::new(),
        });
    }

    let params = json!({
        "name": descriptor.raw_name,
        "arguments": args,
    });
    let call_result = match (isolated_session, policy) {
        (true, Some(policy)) => {
            call_server_once_for_policy(
                &descriptor.server_name,
                config,
                workspace,
                policy,
                "tools/call",
                params,
            )
            .await
        }
        (true, None) => {
            call_server_once(
                &descriptor.server_name,
                config,
                workspace,
                "tools/call",
                params,
            )
            .await
        }
        (false, Some(policy)) => {
            call_server_for_policy(
                &descriptor.server_name,
                config,
                workspace,
                policy,
                "tools/call",
                params,
            )
            .await
        }
        (false, None) => {
            call_server(
                &descriptor.server_name,
                config,
                workspace,
                "tools/call",
                params,
            )
            .await
        }
    };

    let duration_ms = start.elapsed().as_millis() as u64;
    match call_result {
        Ok(result) => {
            let is_error = result
                .get("isError")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if let Some(image_budget) = image_budget.as_ref() {
                image_budget.wait_for_turn().await;
            }
            let rendered = render_call_result_with_image_budget(&result, image_budget.as_ref());
            Some(ToolOutcome {
                output: rendered.output,
                is_error,
                duration_ms,
                subagent_snapshot: None,
                images: rendered.images,
            })
        }
        Err(error) => Some(ToolOutcome {
            output: format!("{name} error: {error}"),
            is_error: true,
            duration_ms,
            subagent_snapshot: None,
            images: Vec::new(),
        }),
    }
}

async fn list_server_catalog_uncached(
    server_name: &str,
    config: &Config,
    workspace: &Path,
) -> McpServerCatalogLoad {
    list_server_catalog_uncached_for_scope(server_name, config, workspace, None).await
}

async fn list_server_catalog_uncached_for_policy(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> McpServerCatalogLoad {
    list_server_catalog_uncached_for_scope(server_name, config, workspace, Some(policy)).await
}

async fn list_server_catalog_uncached_for_scope(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
) -> McpServerCatalogLoad {
    let mut successful_lists = 0;
    let mut errors = Vec::new();

    let (tools, tools_loaded) =
        match list_server_tools_uncached_for_scope(server_name, config, workspace, policy).await {
            Ok(tools) => {
                successful_lists += 1;
                (tools, true)
            }
            Err(error) => {
                errors.push(format!("tools/list: {error}"));
                (Vec::new(), false)
            }
        };
    let (resources, resources_loaded) = match list_server_resources_uncached_for_scope(
        server_name,
        config,
        workspace,
        policy,
    )
    .await
    {
        Ok(resources) => {
            successful_lists += 1;
            (resources, true)
        }
        Err(error) => {
            errors.push(format!("resources/list: {error}"));
            (Vec::new(), false)
        }
    };
    let (prompts, prompts_loaded) = match list_server_prompts_uncached_for_scope(
        server_name,
        config,
        workspace,
        policy,
    )
    .await
    {
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
    inspect_servers_for_scope(config, workspace, None).await
}

pub(crate) async fn inspect_servers_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Vec<McpServerLoadReport> {
    inspect_servers_for_scope(config, workspace, Some(policy)).await
}

async fn inspect_servers_for_scope(
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
) -> Vec<McpServerLoadReport> {
    let mut server_names: Vec<&str> = config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
        .map(|(name, _)| name.as_str())
        .collect();
    server_names.sort_unstable();

    join_all(server_names.into_iter().map(|server_name| async move {
        let catalog = match policy {
            Some(policy) => {
                list_server_catalog_uncached_for_policy(server_name, config, workspace, policy)
                    .await
            }
            None => list_server_catalog_uncached(server_name, config, workspace).await,
        };
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

#[cfg(test)]
pub(crate) async fn catalog_snapshot(config: &Config, workspace: &Path) -> McpCatalogSnapshot {
    catalog_snapshot_for_scope(config, workspace, None).await
}

pub(crate) async fn catalog_snapshot_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> McpCatalogSnapshot {
    catalog_snapshot_for_scope(config, workspace, Some(policy)).await
}

async fn catalog_snapshot_for_scope(
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
) -> McpCatalogSnapshot {
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

        let catalog = match policy {
            Some(policy) => {
                list_server_catalog_uncached_for_policy(server_name, config, workspace, policy)
                    .await
            }
            None => list_server_catalog_uncached(server_name, config, workspace).await,
        };
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
#[cfg(test)]
pub(crate) async fn test_mcp_server(
    server_name: &str,
    mcp_cfg: &JsonMcpServerConfig,
    workspace: &Path,
    default_tool_timeout: Duration,
) -> Result<usize, String> {
    test_mcp_server_for_scope(server_name, mcp_cfg, workspace, default_tool_timeout, None).await
}

pub(crate) async fn test_mcp_server_for_policy(
    server_name: &str,
    mcp_cfg: &JsonMcpServerConfig,
    workspace: &Path,
    default_tool_timeout: Duration,
    policy: &McpSessionPolicy,
) -> Result<usize, String> {
    test_mcp_server_for_scope(
        server_name,
        mcp_cfg,
        workspace,
        default_tool_timeout,
        Some(policy),
    )
    .await
}

async fn test_mcp_server_for_scope(
    server_name: &str,
    mcp_cfg: &JsonMcpServerConfig,
    workspace: &Path,
    default_tool_timeout: Duration,
    policy: Option<&McpSessionPolicy>,
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
        explicit_primary_model_configured: true,
        provider_catalog_declared: false,
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
        enable_task_plan: true,
        enable_groups: true,
        s3: None,
    };
    let tools =
        list_server_tools_uncached_for_scope(server_name, &temp_config, workspace, policy).await?;
    Ok(tools.len())
}

#[cfg(test)]
pub(crate) async fn refresh_servers(
    config: &Config,
    workspace: &Path,
) -> Result<Vec<McpServerLoadReport>, String> {
    refresh_servers_for_scope(config, workspace, None).await
}

pub(crate) async fn refresh_servers_for_policy(
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Result<Vec<McpServerLoadReport>, String> {
    refresh_servers_for_scope(config, workspace, Some(policy)).await
}

async fn refresh_servers_for_scope(
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
) -> Result<Vec<McpServerLoadReport>, String> {
    refresh_server_caches_for_scope(config, workspace, policy).await?;

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
        let cache_key = match policy {
            Some(policy) => cache_key_for_policy(server_name, server, workspace, config, policy),
            None => cache_key(server_name, server, workspace, config),
        };
        let descriptor_permit = if is_streamable_http_server(server) {
            cache_key
                .as_deref()
                .ok()
                .map(capture_http_descriptor_cache_permit)
                .transpose()?
        } else {
            None
        };
        let catalog = match policy {
            Some(policy) => {
                list_server_catalog_uncached_for_policy(server_name, config, workspace, policy)
                    .await
            }
            None => list_server_catalog_uncached(server_name, config, workspace).await,
        };
        match cache_key {
            Ok(cache_key) => {
                let now = Instant::now();
                let http_authority = descriptor_permit
                    .as_ref()
                    .map(|permit| permit.authority.clone());
                #[cfg(test)]
                if http_authority.is_some() {
                    wait_for_http_descriptor_insert_barrier(&cache_key, "catalog").await;
                }
                if catalog.tools_loaded {
                    let inserted =
                        insert_descriptor_cache_if_current(http_authority.as_ref(), || {
                            let mut cache = tool_cache()
                                .lock()
                                .map_err(|_| "MCP tool cache lock poisoned".to_string())?;
                            cache.insert(
                                cache_key.clone(),
                                CachedToolDescriptors {
                                    descriptors: catalog.tools.clone(),
                                    loaded_at: now,
                                    http_authority: http_authority.clone(),
                                },
                            );
                            Ok(())
                        })?;
                    if !inserted {
                        return Err(
                            "HTTP MCP catalog was superseded before tools caching".to_string()
                        );
                    }
                }
                if catalog.resources_loaded {
                    let inserted =
                        insert_descriptor_cache_if_current(http_authority.as_ref(), || {
                            let mut cache = resource_cache()
                                .lock()
                                .map_err(|_| "MCP resource cache lock poisoned".to_string())?;
                            cache.insert(
                                cache_key.clone(),
                                CachedResourceDescriptors {
                                    descriptors: catalog.resources.clone(),
                                    loaded_at: now,
                                    http_authority: http_authority.clone(),
                                },
                            );
                            Ok(())
                        })?;
                    if !inserted {
                        return Err(
                            "HTTP MCP catalog was superseded before resources caching".to_string()
                        );
                    }
                }
                if catalog.prompts_loaded {
                    let inserted =
                        insert_descriptor_cache_if_current(http_authority.as_ref(), || {
                            let mut cache = prompt_cache()
                                .lock()
                                .map_err(|_| "MCP prompt cache lock poisoned".to_string())?;
                            cache.insert(
                                cache_key.clone(),
                                CachedPromptDescriptors {
                                    descriptors: catalog.prompts.clone(),
                                    loaded_at: now,
                                    http_authority: http_authority.clone(),
                                },
                            );
                            Ok(())
                        })?;
                    if !inserted {
                        return Err(
                            "HTTP MCP catalog was superseded before prompts caching".to_string()
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
    if let Ok(mut failures) = spawn_failures().lock() {
        failures.clear();
    }

    let stream_tasks = match http_runtime_state().lock() {
        Ok(mut state) => {
            let mut known_keys = state.controls.keys().cloned().collect::<HashSet<_>>();
            known_keys.extend(state.sessions.keys().cloned());
            known_keys.extend(state.last_event_ids.keys().cloned());
            known_keys.extend(state.stream_tasks.keys().cloned());
            known_keys.extend(state.cleanups.keys().cloned());
            known_keys.extend(state.cleanup_tasks.keys().cloned());
            for cache_key in known_keys {
                let control = state
                    .controls
                    .entry(cache_key.clone())
                    .or_insert_with(|| Arc::new(HttpSessionControl::new()))
                    .clone();
                if is_http_remote_cleanup_domain_key(&cache_key)
                    && control.leases.load(Ordering::Acquire) != 0
                {
                    ensure_http_cleanup_uncertain_locked(&mut state, &cache_key, false);
                }
                control
                    .epoch
                    .store(next_http_session_epoch(), Ordering::Release);
            }
            state.sessions.clear();
            state.last_event_ids.clear();
            state.supersessions.clear();
            let tasks = state
                .stream_tasks
                .drain()
                .map(|(_, entry)| entry.handle)
                .collect::<Vec<_>>();
            let protected_controls = state
                .cleanups
                .keys()
                .chain(state.cleanup_tasks.keys())
                .cloned()
                .collect::<HashSet<_>>();
            let active_cache_keys = state
                .controls
                .iter()
                .filter(|(cache_key, control)| {
                    !is_http_remote_cleanup_domain_key(cache_key)
                        && (control.leases.load(Ordering::Acquire) != 0
                            || protected_controls.contains(*cache_key))
                })
                .map(|(cache_key, _)| cache_key.clone())
                .collect::<HashSet<_>>();
            let active_remote_domains = state
                .remote_domain_by_cache_key
                .iter()
                .filter(|(cache_key, _)| active_cache_keys.contains(*cache_key))
                .map(|(_, remote_domain)| remote_domain.clone())
                .collect::<HashSet<_>>();
            state.controls.retain(|cache_key, control| {
                protected_controls.contains(cache_key)
                    || active_remote_domains.contains(cache_key)
                    || control.leases.load(Ordering::Acquire) != 0
            });
            let retained_controls = state.controls.keys().cloned().collect::<HashSet<_>>();
            state
                .remote_domain_by_cache_key
                .retain(|cache_key, remote_domain| {
                    retained_controls.contains(cache_key)
                        && retained_controls.contains(remote_domain)
                });
            for domains in state.remote_domains_by_server.values_mut() {
                domains.retain(|cache_key| retained_controls.contains(cache_key));
            }
            state
                .remote_domains_by_server
                .retain(|_, domains| !domains.is_empty());
            tasks
        }
        Err(_) => Vec::new(),
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

fn cached_descriptor_authority_is_current(
    server: &JsonMcpServerConfig,
    authority: Option<&HttpDescriptorCacheAuthority>,
) -> bool {
    if is_streamable_http_server(server) {
        authority.is_some_and(http_descriptor_authority_is_current)
    } else {
        authority.is_none()
    }
}

fn remove_tool_cache_entry_if_unchanged(
    cache_key: &str,
    loaded_at: Instant,
    authority: Option<&HttpDescriptorCacheAuthority>,
) {
    if let Ok(mut cache) = tool_cache().lock()
        && cache.get(cache_key).is_some_and(|entry| {
            entry.loaded_at == loaded_at && entry.http_authority.as_ref() == authority
        })
    {
        cache.remove(cache_key);
    }
}

fn insert_descriptor_cache_if_current(
    authority: Option<&HttpDescriptorCacheAuthority>,
    insert: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    let Some(authority) = authority else {
        insert()?;
        return Ok(true);
    };
    let state = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
    if !http_descriptor_authority_is_current_locked(&state, authority) {
        return Ok(false);
    }
    // Lock order is always HTTP runtime state -> one descriptor cache. Cache
    // readers release their cache mutex before validating runtime authority.
    insert()?;
    Ok(true)
}

fn http_runtime_state() -> &'static Mutex<HttpRuntimeState> {
    MCP_HTTP_RUNTIME_STATE.get_or_init(|| Mutex::new(HttpRuntimeState::default()))
}

#[cfg(test)]
struct HttpSessionCacheTestAccessor;

#[cfg(test)]
struct HttpSessionCacheTestGuard(std::sync::MutexGuard<'static, HttpRuntimeState>);

#[cfg(test)]
impl std::ops::Deref for HttpSessionCacheTestGuard {
    type Target = HashMap<String, CachedHttpMcpSession>;

    fn deref(&self) -> &Self::Target {
        &self.0.sessions
    }
}

#[cfg(test)]
impl std::ops::DerefMut for HttpSessionCacheTestGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0.sessions
    }
}

#[cfg(test)]
impl HttpSessionCacheTestAccessor {
    fn lock(&self) -> Result<HttpSessionCacheTestGuard, ()> {
        http_runtime_state()
            .lock()
            .map(HttpSessionCacheTestGuard)
            .map_err(|_| ())
    }
}

#[cfg(test)]
fn http_session_cache() -> HttpSessionCacheTestAccessor {
    HttpSessionCacheTestAccessor
}

#[cfg(test)]
struct HttpStreamTasksTestAccessor;

#[cfg(test)]
struct HttpStreamTasksTestGuard(std::sync::MutexGuard<'static, HttpRuntimeState>);

#[cfg(test)]
impl std::ops::Deref for HttpStreamTasksTestGuard {
    type Target = HashMap<String, HttpStreamTaskEntry>;

    fn deref(&self) -> &Self::Target {
        &self.0.stream_tasks
    }
}

#[cfg(test)]
impl std::ops::DerefMut for HttpStreamTasksTestGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0.stream_tasks
    }
}

#[cfg(test)]
impl HttpStreamTasksTestAccessor {
    fn lock(&self) -> Result<HttpStreamTasksTestGuard, ()> {
        http_runtime_state()
            .lock()
            .map(HttpStreamTasksTestGuard)
            .map_err(|_| ())
    }
}

#[cfg(test)]
fn http_stream_tasks() -> HttpStreamTasksTestAccessor {
    HttpStreamTasksTestAccessor
}

fn next_http_stream_task_id() -> u64 {
    MCP_NEXT_HTTP_STREAM_TASK_ID.fetch_add(1, Ordering::Relaxed)
}

fn next_http_cleanup_id() -> u64 {
    MCP_NEXT_HTTP_CLEANUP_ID.fetch_add(1, Ordering::Relaxed)
}

fn next_http_session_generation() -> u64 {
    MCP_NEXT_HTTP_SESSION_GENERATION.fetch_add(1, Ordering::Relaxed)
}

fn next_http_session_epoch() -> u64 {
    MCP_NEXT_HTTP_SESSION_EPOCH.fetch_add(1, Ordering::Relaxed)
}

fn next_http_request_id() -> u64 {
    MCP_NEXT_HTTP_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
}

fn http_control_lease(cache_key: &str) -> Result<HttpControlLease, String> {
    let mut state = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
    let control = state
        .controls
        .entry(cache_key.to_string())
        .or_insert_with(|| Arc::new(HttpSessionControl::new()))
        .clone();
    control.leases.fetch_add(1, Ordering::AcqRel);
    Ok(HttpControlLease {
        cache_key: cache_key.to_string(),
        control,
    })
}

async fn http_key_exclusive_guard(cache_key: &str) -> Result<HttpKeyExclusiveGuard, String> {
    let lease = http_control_lease(cache_key)?;
    let lock = lease.control.exclusive.clone().lock_owned();
    #[cfg(not(test))]
    let guard = lock.await;
    #[cfg(test)]
    let guard = {
        use std::{future::Future as _, task::Poll};

        let mut wait_signal = MCP_HTTP_EXCLUSIVE_WAIT_SIGNALS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .map_err(|_| "HTTP MCP exclusive wait signal lock poisoned".to_string())?
            .remove(cache_key);
        let mut lock = Box::pin(lock);
        std::future::poll_fn(move |cx| match lock.as_mut().poll(cx) {
            Poll::Ready(guard) => Poll::Ready(guard),
            Poll::Pending => {
                if let Some(signal) = wait_signal.take() {
                    let _ = signal.send(());
                }
                Poll::Pending
            }
        })
        .await
    };
    Ok(HttpKeyExclusiveGuard {
        _guard: guard,
        lease,
    })
}

#[cfg(test)]
fn install_http_exclusive_wait_signal(cache_key: &str, signal: tokio::sync::oneshot::Sender<()>) {
    MCP_HTTP_EXCLUSIVE_WAIT_SIGNALS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("HTTP MCP exclusive wait signal lock")
        .insert(cache_key.to_string(), signal);
}

#[cfg(test)]
fn install_http_descriptor_insert_barrier(
    cache_key: &str,
    cache_kind: &'static str,
    reached: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
) {
    *MCP_HTTP_DESCRIPTOR_INSERT_BARRIER
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("HTTP MCP descriptor insert barrier lock") =
        Some(HttpDescriptorInsertTestBarrier {
            cache_key: cache_key.to_string(),
            cache_kind,
            reached: Some(reached),
            release,
        });
}

#[cfg(test)]
async fn wait_for_http_descriptor_insert_barrier(cache_key: &str, cache_kind: &'static str) {
    let barrier = {
        let mut slot = MCP_HTTP_DESCRIPTOR_INSERT_BARRIER
            .get_or_init(|| Mutex::new(None))
            .lock()
            .expect("HTTP MCP descriptor insert barrier lock");
        if slot.as_ref().is_some_and(|barrier| {
            barrier.cache_key == cache_key && barrier.cache_kind == cache_kind
        }) {
            slot.take()
        } else {
            None
        }
    };
    if let Some(mut barrier) = barrier {
        if let Some(reached) = barrier.reached.take() {
            let _ = reached.send(());
        }
        let _ = barrier.release.await;
    }
}

const HTTP_REMOTE_CLEANUP_DOMAIN_PREFIX: &str = "http-remote-cleanup-domain\n";

fn canonicalize_endpoint_percent_encoding(value: &str) -> Result<String, String> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    fn hex_value(value: u8) -> Option<u8> {
        match value {
            b'0'..=b'9' => Some(value - b'0'),
            b'a'..=b'f' => Some(value - b'a' + 10),
            b'A'..=b'F' => Some(value - b'A' + 10),
            _ => None,
        }
    }

    fn is_unreserved(value: u8) -> bool {
        value.is_ascii_alphanumeric() || matches!(value, b'-' | b'.' | b'_' | b'~')
    }

    let bytes = value.as_bytes();
    let mut canonical = String::with_capacity(value.len());
    let mut copied_from = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        canonical.push_str(&value[copied_from..index]);
        let Some(high) = bytes.get(index + 1).and_then(|value| hex_value(*value)) else {
            return Err("invalid percent escape in HTTP MCP endpoint".to_string());
        };
        let Some(low) = bytes.get(index + 2).and_then(|value| hex_value(*value)) else {
            return Err("invalid percent escape in HTTP MCP endpoint".to_string());
        };
        let decoded = (high << 4) | low;
        if is_unreserved(decoded) {
            canonical.push(char::from(decoded));
        } else {
            canonical.push('%');
            canonical.push(char::from(HEX[usize::from(decoded >> 4)]));
            canonical.push(char::from(HEX[usize::from(decoded & 0x0f)]));
        }
        index += 3;
        copied_from = index;
    }
    canonical.push_str(&value[copied_from..]);
    Ok(canonical)
}

fn streamable_http_endpoint_url(server: &JsonMcpServerConfig) -> Result<reqwest::Url, String> {
    let url = server
        .url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "HTTP MCP server is missing streamable-http url".to_string())?;
    let mut url = reqwest::Url::parse(url)
        .map_err(|error| format!("invalid HTTP MCP server url: {error}"))?;
    let canonical_path = canonicalize_endpoint_percent_encoding(url.path())?;
    let canonical_query = url
        .query()
        .map(canonicalize_endpoint_percent_encoding)
        .transpose()?;
    url.set_path(&canonical_path);
    url.set_query(canonical_query.as_deref());
    url.set_fragment(None);
    Ok(url)
}

fn http_remote_cleanup_domain_key(server: &JsonMcpServerConfig) -> Result<String, String> {
    let mut url = streamable_http_endpoint_url(server)?;
    url.set_username("")
        .map_err(|_| "invalid HTTP MCP server url userinfo".to_string())?;
    url.set_password(None)
        .map_err(|_| "invalid HTTP MCP server url userinfo".to_string())?;
    let endpoint_digest = URL_SAFE_NO_PAD.encode(Sha256::digest(url.as_str().as_bytes()));
    Ok(format!(
        "{HTTP_REMOTE_CLEANUP_DOMAIN_PREFIX}{endpoint_digest}"
    ))
}

fn is_http_remote_cleanup_domain_key(cache_key: &str) -> bool {
    cache_key.starts_with(HTTP_REMOTE_CLEANUP_DOMAIN_PREFIX)
}

fn one_shot_session_cache_key(base_key: &str) -> String {
    format!("{base_key}\none-shot-session")
}

fn http_remote_domain_for_cache_key(
    cache_key: &str,
    server: &JsonMcpServerConfig,
) -> Result<String, String> {
    if is_http_remote_cleanup_domain_key(cache_key) {
        return Ok(cache_key.to_string());
    }
    if let Ok(state) = http_runtime_state().lock()
        && let Some(remote_domain) = state.remote_domain_by_cache_key.get(cache_key)
    {
        return Ok(remote_domain.clone());
    }
    http_remote_cleanup_domain_key(server)
}

async fn http_remote_cleanup_guard(
    server_name: &str,
    server: &JsonMcpServerConfig,
    session_cache_key: &str,
) -> Result<(HttpKeyExclusiveGuard, HttpOneShotScopeAuthority), String> {
    http_remote_cleanup_guard_inner(server_name, server, session_cache_key, false).await
}

async fn http_remote_cleanup_guard_after_pending(
    server_name: &str,
    server: &JsonMcpServerConfig,
    session_cache_key: &str,
) -> Result<(HttpKeyExclusiveGuard, HttpOneShotScopeAuthority), String> {
    http_remote_cleanup_guard_inner(server_name, server, session_cache_key, true).await
}

async fn http_remote_cleanup_guard_inner(
    server_name: &str,
    server: &JsonMcpServerConfig,
    session_cache_key: &str,
    wait_for_pending: bool,
) -> Result<(HttpKeyExclusiveGuard, HttpOneShotScopeAuthority), String> {
    let cache_key = http_remote_domain_for_cache_key(session_cache_key, server)?;
    // Ordinary initialization fails immediately behind any cleanup. The
    // production idle path may instead wait behind a pending DELETE while its
    // caller still owns this endpoint guard, then recheck under the guard so
    // concurrent idle callers share the confirmed cleanup/reinitialize flow.
    // Uncertain cleanup always fails immediately.
    if (wait_for_pending && http_cleanup_is_uncertain(&cache_key))
        || (!wait_for_pending && http_cleanup_blocks_initialize(&cache_key))
    {
        return Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string());
    }
    let guard = http_key_exclusive_guard(&cache_key).await?;
    let mut state = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
    let epoch = guard.lease.control.epoch.load(Ordering::Acquire);
    if !http_control_is_current_locked(&state, &cache_key, &guard.lease, epoch) {
        return Err("HTTP MCP one-shot scope authority was superseded".to_string());
    }
    if state.cleanups.contains_key(&cache_key) {
        return Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string());
    }
    state
        .remote_domains_by_server
        .entry(server_name.to_string())
        .or_default()
        .insert(cache_key.clone());
    if !is_http_remote_cleanup_domain_key(session_cache_key) {
        match state.remote_domain_by_cache_key.get(session_cache_key) {
            Some(existing) if existing != &cache_key => {
                return Err("HTTP MCP cache key changed remote cleanup domain".to_string());
            }
            Some(_) => {}
            None => {
                state
                    .remote_domain_by_cache_key
                    .insert(session_cache_key.to_string(), cache_key.clone());
            }
        }
    }
    drop(state);
    let authority = HttpOneShotScopeAuthority {
        cache_key,
        control: guard.lease.clone(),
        epoch,
    };
    Ok((guard, authority))
}

fn try_reclaim_http_control(cache_key: &str, control: &Arc<HttpSessionControl>) {
    let Ok(mut state) = http_runtime_state().lock() else {
        return;
    };
    if control.leases.load(Ordering::Acquire) != 0
        || state.sessions.contains_key(cache_key)
        || state.last_event_ids.contains_key(cache_key)
        || state.stream_tasks.contains_key(cache_key)
        || state.cleanups.contains_key(cache_key)
        || state.cleanup_tasks.contains_key(cache_key)
        || state
            .in_flight_requests
            .keys()
            .any(|key| key.cache_key == cache_key)
        || state
            .deferred_cleanups
            .keys()
            .any(|key| key.cache_key == cache_key)
        || state
            .remote_domain_by_cache_key
            .values()
            .any(|remote_domain| remote_domain == cache_key)
        || !state
            .controls
            .get(cache_key)
            .is_some_and(|current| Arc::ptr_eq(current, control))
    {
        return;
    }
    state.controls.remove(cache_key);
    let associated_remote_domain = state.remote_domain_by_cache_key.remove(cache_key);
    for domains in state.remote_domains_by_server.values_mut() {
        domains.remove(cache_key);
    }
    state
        .remote_domains_by_server
        .retain(|_, domains| !domains.is_empty());
    let associated_remote_control = associated_remote_domain.and_then(|remote_domain| {
        state
            .controls
            .get(&remote_domain)
            .cloned()
            .map(|control| (remote_domain, control))
    });
    drop(state);
    if let Some((remote_domain, remote_control)) = associated_remote_control {
        try_reclaim_http_control(&remote_domain, &remote_control);
    }
}

impl Drop for HttpControlLease {
    fn drop(&mut self) {
        if self.control.leases.fetch_sub(1, Ordering::AcqRel) == 1 {
            try_reclaim_http_control(&self.cache_key, &self.control);
        }
    }
}

impl Drop for HttpInFlightRequestLease {
    fn drop(&mut self) {
        let notify = {
            let Ok(mut state) = http_runtime_state().lock() else {
                return;
            };
            let should_remove = match state.in_flight_requests.get_mut(&self.key) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => true,
                None => false,
            };
            if should_remove {
                state.in_flight_requests.remove(&self.key);
            }

            let deferred = state.deferred_cleanups.get_mut(&self.key).map(|cleanup| {
                if !self.completed {
                    cleanup.force_uncertain = true;
                }
                (
                    cleanup.cleanup_id,
                    cleanup.remote_cleanup.clone(),
                    cleanup.notify.clone(),
                )
            });
            if !self.completed
                && let Some((cleanup_id, remote_cleanup, _)) = deferred.as_ref()
            {
                mark_http_cleanup_uncertain_locked(
                    &mut state,
                    &self.key.cache_key,
                    *cleanup_id,
                    true,
                );
                if let Some((remote_cache_key, remote_cleanup_id)) = remote_cleanup {
                    mark_http_cleanup_uncertain_locked(
                        &mut state,
                        remote_cache_key,
                        *remote_cleanup_id,
                        true,
                    );
                }
            }
            should_remove
                .then(|| deferred.map(|(_, _, notify)| notify))
                .flatten()
        };
        if let Some(notify) = notify {
            notify.notify_one();
        }
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

#[derive(Debug)]
struct RenderedCallResult {
    output: String,
    images: Vec<ToolImageOutput>,
}

fn mcp_image_name(value: &Value, index: usize, mime_type: &str) -> String {
    value
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| {
            value
                .get("uri")
                .and_then(Value::as_str)
                .and_then(|uri| uri.rsplit('/').next())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            let extension = if mime_type == "image/png" {
                "png"
            } else {
                "jpg"
            };
            format!("mcp-image-{}.{}", index + 1, extension)
        })
}

fn extract_mcp_image(
    source: &Value,
    encoded: &str,
    declared_mime: &str,
    index: usize,
    image_budget: &ToolImageBudget,
) -> Result<ToolImageOutput, String> {
    let normalized_mime = declared_mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if !crate::image_uploads::is_supported_image_content_type(&normalized_mime) {
        return Err(format!("unsupported image type '{declared_mime}'"));
    }
    if encoded.len() > crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES.saturating_mul(2) {
        return Err("encoded image exceeds the 10 MB limit".to_string());
    }
    if !image_budget.try_reserve() {
        return Err("tool image batch limit reached".to_string());
    }

    let extracted = (|| {
        let data = STANDARD
            .decode(encoded)
            .map_err(|_| "invalid Base64 image data".to_string())?;
        if data.len() > crate::image_uploads::MAX_IMAGE_UPLOAD_BYTES {
            return Err("decoded image exceeds the 10 MB limit".to_string());
        }
        let actual_mime = crate::image_uploads::detect_image_upload_content_type(&data)
            .ok_or_else(|| "image bytes are not a valid PNG or JPEG".to_string())?;
        let declared_matches = normalized_mime == actual_mime
            || (normalized_mime == "image/jpg" && actual_mime == "image/jpeg");
        if !declared_matches {
            return Err(format!(
                "declared image type '{declared_mime}' does not match '{actual_mime}'"
            ));
        }
        Ok(ToolImageOutput {
            name: mcp_image_name(source, index, actual_mime),
            mime_type: actual_mime.to_string(),
            data,
        })
    })();
    if extracted.is_err() {
        image_budget.release();
    }
    extracted
}

fn looks_like_base64_payload(value: &str) -> bool {
    let value = value.trim();
    value.len() >= 32
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'+' | b'/' | b'=' | b'-' | b'_' | b'\r' | b'\n')
        })
}

fn redact_known_image_payloads(text: &str, known_image_payloads: &[&str]) -> String {
    let mut redacted = text.to_string();
    for payload in known_image_payloads
        .iter()
        .copied()
        .filter(|payload| !payload.is_empty())
    {
        if redacted.contains(payload) {
            redacted = redacted.replace(payload, "[binary data omitted]");
        }
    }
    redacted
}

fn structured_object_is_binary(object: &serde_json::Map<String, Value>) -> bool {
    let content_type = object
        .get("mimeType")
        .or_else(|| object.get("mime_type"))
        .or_else(|| object.get("contentType"))
        .or_else(|| object.get("content_type"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let item_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let encoding = object
        .get("encoding")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();

    matches!(item_type.as_str(), "image" | "audio" | "binary")
        || (!content_type.is_empty()
            && !content_type.starts_with("text/")
            && content_type != "application/json")
        || encoding.eq_ignore_ascii_case("base64")
}

fn redact_structured_binary_payloads(
    value: &Value,
    known_image_payloads: &[&str],
    key_hint: Option<&str>,
    binary_context: bool,
) -> Value {
    match value {
        Value::String(text) => {
            let redacted = redact_known_image_payloads(text, known_image_payloads);
            if redacted != *text {
                return Value::String(redacted);
            }
            let key = key_hint.unwrap_or("");
            let lower = text.trim_start().to_ascii_lowercase();
            let binary_key = matches!(
                key.to_ascii_lowercase().as_str(),
                "blob" | "base64" | "bytes" | "binary"
            );
            let data_key = key.eq_ignore_ascii_case("data");
            let ordinary_text = matches!(key.to_ascii_lowercase().as_str(), "text" | "content");
            let data_url =
                !ordinary_text && lower.starts_with("data:") && lower.contains(";base64,");
            let encoded_like = looks_like_base64_payload(text);
            let generic_binary_signal = text.bytes().any(|byte| matches!(byte, b'+' | b'/' | b'='))
                || (text.trim().len() >= 128
                    && !text.trim().bytes().all(|byte| byte.is_ascii_hexdigit()));
            let encoded_payload = !ordinary_text
                && ((data_key && binary_context)
                    || (encoded_like && (data_key || binary_context || generic_binary_signal)));
            if binary_key || data_url || encoded_payload {
                Value::String("[binary data omitted]".to_string())
            } else {
                value.clone()
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| {
                    redact_structured_binary_payloads(
                        item,
                        known_image_payloads,
                        key_hint,
                        binary_context,
                    )
                })
                .collect(),
        ),
        Value::Object(object) => {
            let object_is_binary = binary_context || structured_object_is_binary(object);
            Value::Object(
                object
                    .iter()
                    .map(|(key, item)| {
                        (
                            key.clone(),
                            redact_structured_binary_payloads(
                                item,
                                known_image_payloads,
                                Some(key),
                                object_is_binary,
                            ),
                        )
                    })
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}

fn sanitized_mcp_value(value: &Value, known_image_payloads: &[&str]) -> Value {
    redact_structured_binary_payloads(value, known_image_payloads, None, false)
}

fn collect_mcp_image_payloads(result: &Value) -> Vec<&str> {
    result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| match item.get("type").and_then(Value::as_str) {
            Some("image") => item.get("data").and_then(Value::as_str),
            Some("resource") => item
                .get("resource")
                .and_then(|resource| resource.get("blob"))
                .and_then(Value::as_str),
            _ => None,
        })
        .collect()
}

fn render_call_result_with_image_budget(
    result: &Value,
    shared_budget: Option<&ToolImageBudget>,
) -> RenderedCallResult {
    let mut parts = Vec::new();
    let mut images = Vec::new();
    // Collect standard image payloads before rendering any text so an MCP
    // result cannot leak the same Base64 merely by placing a text item first.
    let image_payloads = collect_mcp_image_payloads(result);
    let local_budget;
    let image_budget = match shared_budget {
        Some(budget) => budget,
        None => {
            local_budget = ToolImageBudget::new(crate::image_uploads::MAX_IMAGE_UPLOAD_FILES);
            &local_budget
        }
    };

    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for item in content {
            if let Some(text) = item.get("text").and_then(Value::as_str)
                && !text.is_empty()
            {
                parts.push(redact_known_image_payloads(text, &image_payloads));
                continue;
            }
            let item_type = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let image_source = match item_type {
                "image" => item.get("data").and_then(Value::as_str).map(|data| {
                    (
                        item,
                        data,
                        item.get("mimeType").and_then(Value::as_str).unwrap_or(""),
                    )
                }),
                "resource" => item.get("resource").and_then(|resource| {
                    resource.get("blob").and_then(Value::as_str).map(|data| {
                        (
                            resource,
                            data,
                            resource
                                .get("mimeType")
                                .and_then(Value::as_str)
                                .unwrap_or(""),
                        )
                    })
                }),
                _ => None,
            };
            if let Some((source, encoded, mime_type)) = image_source {
                match extract_mcp_image(source, encoded, mime_type, images.len(), image_budget) {
                    Ok(image) => {
                        parts.push(format!(
                            "[image output: {} ({})]",
                            image.name, image.mime_type
                        ));
                        images.push(image);
                    }
                    Err(error) => parts.push(format!("[image not attached: {error}]")),
                }
                continue;
            }
            if item_type == "resource"
                && let Some(resource) = item.get("resource")
            {
                if let Some(text) = resource.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                } else if resource.get("blob").is_some() {
                    parts.push("[binary resource omitted]".to_string());
                } else {
                    parts.push("[resource metadata received]".to_string());
                }
                continue;
            }
            let sanitized = sanitized_mcp_value(item, &image_payloads);
            parts.push(format!(
                "[{item_type}] {}",
                serde_json::to_string_pretty(&sanitized).unwrap_or_else(|_| sanitized.to_string())
            ));
        }
    }

    if let Some(structured) = result.get("structuredContent") {
        let structured = sanitized_mcp_value(structured, &image_payloads);
        parts.push(format!(
            "structuredContent:\n{}",
            serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string())
        ));
    }

    let output = if parts.is_empty() {
        let sanitized = sanitized_mcp_value(result, &image_payloads);
        serde_json::to_string_pretty(&sanitized).unwrap_or_else(|_| sanitized.to_string())
    } else {
        parts.join("\n\n")
    };
    RenderedCallResult { output, images }
}

#[cfg(test)]
fn render_call_result(result: &Value) -> RenderedCallResult {
    render_call_result_with_image_budget(result, None)
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
            list_server_tools_for_policy(server_name, config, workspace, policy).await,
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
    list_tools_for_servers_with_status_inner(config, workspace, server_names, None, false).await
}

pub(crate) async fn list_tools_for_servers_uncached_with_status_for_policy(
    config: &Config,
    workspace: &Path,
    server_names: &HashSet<String>,
    policy: &McpSessionPolicy,
) -> (Vec<McpToolDescriptor>, HashSet<String>) {
    list_tools_for_servers_with_status_inner(config, workspace, server_names, Some(policy), true)
        .await
}

async fn list_tools_for_servers_with_status_inner(
    config: &Config,
    workspace: &Path,
    server_names: &HashSet<String>,
    policy: Option<&McpSessionPolicy>,
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
            list_server_tools_uncached_for_scope(server_name, config, workspace, policy).await
        } else {
            match policy {
                Some(policy) => {
                    list_server_tools_for_policy(server_name, config, workspace, policy).await
                }
                None => list_server_tools(server_name, config, workspace).await,
            }
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
                    Some(entry.clone())
                }
                Some(_) => {
                    cache.remove(&key);
                    None
                }
                None => None,
            }
        };

        if let Some(mut cached) = cached
            && cached_descriptor_authority_is_current(server, cached.http_authority.as_ref())
        {
            tools.append(&mut cached.descriptors);
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

    let mut server_names = policy
        .enabled_servers
        .iter()
        .filter_map(|server_name| {
            config
                .mcp_servers
                .get(server_name)
                .filter(|server| server.enabled)
                .map(|_| server_name.as_str())
        })
        .collect::<Vec<_>>();
    server_names.sort_unstable();

    let now = Instant::now();
    let mut tools = Vec::new();
    for server_name in server_names {
        let Some(server) = config.mcp_servers.get(server_name) else {
            continue;
        };
        let Ok(key) = cache_key_for_policy(server_name, server, workspace, config, policy) else {
            continue;
        };
        let cached = {
            let Ok(mut cache) = tool_cache().lock() else {
                continue;
            };
            match cache.get(&key) {
                Some(entry) if now.duration_since(entry.loaded_at) < tool_cache_ttl() => {
                    Some(entry.clone())
                }
                Some(_) => {
                    cache.remove(&key);
                    None
                }
                None => None,
            }
        };
        if let Some(cached) = cached
            && cached_descriptor_authority_is_current(server, cached.http_authority.as_ref())
        {
            tools.extend(
                cached
                    .descriptors
                    .into_iter()
                    .filter(|tool| policy.allows_tool(tool)),
            );
        }
    }
    tools
}

#[cfg(test)]
pub(crate) fn insert_cached_tool_descriptors_for_test(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    descriptors: Vec<McpToolDescriptor>,
) {
    let server = config
        .mcp_servers
        .get(server_name)
        .expect("test MCP server should exist");
    let key = cache_key(server_name, server, workspace, config).expect("cache key should build");
    tool_cache().lock().expect("tool cache lock").insert(
        key,
        CachedToolDescriptors {
            descriptors,
            loaded_at: Instant::now(),
            http_authority: None,
        },
    );
}

#[cfg(test)]
pub(crate) fn insert_cached_tool_descriptors_for_policy_for_test(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
    descriptors: Vec<McpToolDescriptor>,
) {
    let server = config
        .mcp_servers
        .get(server_name)
        .expect("test MCP server should exist");
    let key = cache_key_for_policy(server_name, server, workspace, config, policy)
        .expect("policy cache key should build");
    tool_cache().lock().expect("tool cache lock").insert(
        key,
        CachedToolDescriptors {
            descriptors,
            loaded_at: Instant::now(),
            http_authority: None,
        },
    );
}

fn cache_key(
    server_name: &str,
    server: &JsonMcpServerConfig,
    workspace: &Path,
    config: &Config,
) -> Result<String, String> {
    let client_capabilities = client_capabilities_for_server(server_name, workspace);
    cache_key_for_scope(
        server_name,
        server,
        workspace,
        workspace,
        config,
        &client_capabilities,
    )
}

fn cache_key_for_policy(
    server_name: &str,
    server: &JsonMcpServerConfig,
    workspace: &Path,
    config: &Config,
    policy: &McpSessionPolicy,
) -> Result<String, String> {
    let client_capabilities = policy.client_capabilities_for_server(server_name);
    cache_key_for_scope(
        server_name,
        server,
        workspace,
        policy.cache_namespace(workspace),
        config,
        &client_capabilities,
    )
}

fn cache_key_for_scope(
    server_name: &str,
    server: &JsonMcpServerConfig,
    workspace: &Path,
    cache_namespace: &Path,
    config: &Config,
    client_capabilities: &McpClientCapabilityPolicy,
) -> Result<String, String> {
    let resolved_cwd = resolve_server_cwd(server, workspace)?;
    let client_capabilities = if is_streamable_http_server(server) {
        effective_http_client_capabilities(client_capabilities)
    } else {
        effective_client_capabilities(client_capabilities)
    };
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
    let capabilities = initialize_capabilities(&client_capabilities);
    let capabilities_key =
        serde_json::to_string(&capabilities).unwrap_or_else(|_| "{}".to_string());
    Ok(format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        server_name,
        server.effective_transport(),
        server.command,
        server.url.as_deref().unwrap_or_default(),
        server.args.join("\u{1f}"),
        resolved_cwd.display_path().display(),
        cache_namespace.display(),
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
        let tools = match policy {
            Some(policy) => {
                list_server_tools_for_policy(server_name, config, workspace, policy).await?
            }
            None => list_server_tools(server_name, config, workspace).await?,
        };
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
    list_server_tools_for_scope(server_name, config, workspace, None).await
}

async fn list_server_tools_for_policy(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Result<Vec<McpToolDescriptor>, String> {
    list_server_tools_for_scope(server_name, config, workspace, Some(policy)).await
}

async fn list_server_tools_for_scope(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
) -> Result<Vec<McpToolDescriptor>, String> {
    let server = config
        .mcp_servers
        .get(server_name)
        .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
    let key = match policy {
        Some(policy) => cache_key_for_policy(server_name, server, workspace, config, policy)?,
        None => cache_key(server_name, server, workspace, config)?,
    };
    let now = Instant::now();

    let cached = {
        let mut cache = tool_cache()
            .lock()
            .map_err(|_| "MCP tool cache lock poisoned".to_string())?;
        match cache.get(&key) {
            Some(entry) if now.duration_since(entry.loaded_at) < tool_cache_ttl() => {
                Some(entry.clone())
            }
            Some(_) => {
                cache.remove(&key);
                None
            }
            None => None,
        }
    };
    if let Some(cached) = cached
        && cached_descriptor_authority_is_current(server, cached.http_authority.as_ref())
    {
        return Ok(cached.descriptors);
    }

    let listed = list_server_items_for_scope_with_authority(
        server_name,
        config,
        workspace,
        policy,
        "tools/list",
        "tools",
        json!({}),
        false,
    )
    .await?;
    let descriptors = parse_tool_descriptors(server_name, &json!({ "tools": listed.items }))?;
    if is_streamable_http_server(server) && listed.descriptor_authority.is_none() {
        return Err("HTTP MCP tools/list response is missing cache authority".to_string());
    }
    #[cfg(test)]
    wait_for_http_descriptor_insert_barrier(&key, "tools").await;
    let inserted =
        insert_descriptor_cache_if_current(listed.descriptor_authority.as_ref(), || {
            let mut cache = tool_cache()
                .lock()
                .map_err(|_| "MCP tool cache lock poisoned".to_string())?;
            cache.insert(
                key,
                CachedToolDescriptors {
                    descriptors: descriptors.clone(),
                    loaded_at: Instant::now(),
                    http_authority: listed.descriptor_authority.clone(),
                },
            );
            Ok(())
        })?;
    if !inserted {
        return Err("HTTP MCP tools/list response was superseded before caching".to_string());
    }

    Ok(descriptors)
}

#[cfg(test)]
async fn list_server_tools_uncached(
    server_name: &str,
    config: &Config,
    workspace: &Path,
) -> Result<Vec<McpToolDescriptor>, String> {
    list_server_tools_uncached_for_scope(server_name, config, workspace, None).await
}

async fn list_server_tools_uncached_for_scope(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
) -> Result<Vec<McpToolDescriptor>, String> {
    let tools = list_server_items_for_scope(
        server_name,
        config,
        workspace,
        policy,
        "tools/list",
        "tools",
        json!({}),
        true,
    )
    .await?;
    parse_tool_descriptors(server_name, &json!({ "tools": tools }))
}

#[cfg(test)]
async fn list_server_items(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    method: &str,
    array_key: &str,
    base_params: Value,
    uncached_session: bool,
) -> Result<Vec<Value>, String> {
    list_server_items_for_scope(
        server_name,
        config,
        workspace,
        None,
        method,
        array_key,
        base_params,
        uncached_session,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn list_server_items_for_scope(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
    method: &str,
    array_key: &str,
    base_params: Value,
    uncached_session: bool,
) -> Result<Vec<Value>, String> {
    list_server_items_for_scope_with_authority(
        server_name,
        config,
        workspace,
        policy,
        method,
        array_key,
        base_params,
        uncached_session,
    )
    .await
    .map(|listed| listed.items)
}

#[allow(clippy::too_many_arguments)]
async fn list_server_items_for_scope_with_authority(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
    method: &str,
    array_key: &str,
    base_params: Value,
    uncached_session: bool,
) -> Result<ListedServerItems, String> {
    if uncached_session {
        return list_server_items_with_temporary_session(
            server_name,
            config,
            workspace,
            policy,
            method,
            array_key,
            base_params,
        )
        .await
        .map(|items| ListedServerItems {
            items,
            descriptor_authority: None,
        });
    }

    let mut cursor: Option<String> = None;
    let mut seen_cursors = HashSet::new();
    let mut items = Vec::new();
    let mut descriptor_authority = None;

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
        let call = call_server_for_scope_with_descriptor_authority(
            server_name,
            config,
            workspace,
            policy,
            method,
            params,
        )
        .await?;
        if let Some(authority) = call.descriptor_authority.as_ref() {
            match descriptor_authority.as_ref() {
                Some(existing) if existing != authority => {
                    return Err(format!(
                        "server '{server_name}' changed HTTP Session generation during {method} pagination"
                    ));
                }
                Some(_) => {}
                None => descriptor_authority = Some(authority.clone()),
            }
        }
        let response = call.value;
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
            return Ok(ListedServerItems {
                items,
                descriptor_authority,
            });
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
    policy: Option<&McpSessionPolicy>,
    method: &str,
    array_key: &str,
    base_params: Value,
) -> Result<Vec<Value>, String> {
    let mut session = match policy {
        Some(policy) => {
            TemporaryMcpSession::new_for_policy(server_name, config, workspace, policy).await?
        }
        None => TemporaryMcpSession::new(server_name, config, workspace).await?,
    };
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
        let annotations = tool.get("annotations");
        descriptors.push(McpToolDescriptor {
            server_name: server_name.to_string(),
            raw_name: raw_name.to_string(),
            exposed_name,
            description,
            input_schema,
            annotations: McpToolAnnotations {
                read_only_hint: annotations
                    .and_then(|value| value.get("readOnlyHint"))
                    .and_then(Value::as_bool),
                destructive_hint: annotations
                    .and_then(|value| value.get("destructiveHint"))
                    .and_then(Value::as_bool),
            },
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
                Some(entry.clone())
            }
            Some(_) => {
                cache.remove(&key);
                None
            }
            None => None,
        }
    };
    if let Some(cached) = cached
        && cached_descriptor_authority_is_current(server, cached.http_authority.as_ref())
    {
        return Ok(cached.descriptors);
    }

    let listed = list_server_items_for_scope_with_authority(
        server_name,
        config,
        workspace,
        None,
        "resources/list",
        "resources",
        json!({}),
        false,
    )
    .await?;
    let descriptors =
        parse_resource_descriptors(server_name, &json!({ "resources": listed.items }))?;
    if is_streamable_http_server(server) && listed.descriptor_authority.is_none() {
        return Err("HTTP MCP resources/list response is missing cache authority".to_string());
    }
    #[cfg(test)]
    wait_for_http_descriptor_insert_barrier(&key, "resources").await;
    let inserted =
        insert_descriptor_cache_if_current(listed.descriptor_authority.as_ref(), || {
            let mut cache = resource_cache()
                .lock()
                .map_err(|_| "MCP resource cache lock poisoned".to_string())?;
            cache.insert(
                key,
                CachedResourceDescriptors {
                    descriptors: descriptors.clone(),
                    loaded_at: Instant::now(),
                    http_authority: listed.descriptor_authority.clone(),
                },
            );
            Ok(())
        })?;
    if !inserted {
        return Err("HTTP MCP resources/list response was superseded before caching".to_string());
    }
    Ok(descriptors)
}

async fn list_server_resources_uncached_for_scope(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
) -> Result<Vec<McpResourceDescriptor>, String> {
    let resources = list_server_items_for_scope(
        server_name,
        config,
        workspace,
        policy,
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
                Some(entry.clone())
            }
            Some(_) => {
                cache.remove(&key);
                None
            }
            None => None,
        }
    };
    if let Some(cached) = cached
        && cached_descriptor_authority_is_current(server, cached.http_authority.as_ref())
    {
        return Ok(cached.descriptors);
    }

    let listed = list_server_items_for_scope_with_authority(
        server_name,
        config,
        workspace,
        None,
        "prompts/list",
        "prompts",
        json!({}),
        false,
    )
    .await?;
    let descriptors = parse_prompt_descriptors(server_name, &json!({ "prompts": listed.items }))?;
    if is_streamable_http_server(server) && listed.descriptor_authority.is_none() {
        return Err("HTTP MCP prompts/list response is missing cache authority".to_string());
    }
    #[cfg(test)]
    wait_for_http_descriptor_insert_barrier(&key, "prompts").await;
    let inserted =
        insert_descriptor_cache_if_current(listed.descriptor_authority.as_ref(), || {
            let mut cache = prompt_cache()
                .lock()
                .map_err(|_| "MCP prompt cache lock poisoned".to_string())?;
            cache.insert(
                key,
                CachedPromptDescriptors {
                    descriptors: descriptors.clone(),
                    loaded_at: Instant::now(),
                    http_authority: listed.descriptor_authority.clone(),
                },
            );
            Ok(())
        })?;
    if !inserted {
        return Err("HTTP MCP prompts/list response was superseded before caching".to_string());
    }
    Ok(descriptors)
}

async fn list_server_prompts_uncached_for_scope(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
) -> Result<Vec<McpPromptDescriptor>, String> {
    let prompts = list_server_items_for_scope(
        server_name,
        config,
        workspace,
        policy,
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

pub(crate) async fn read_resource_for_policy(
    server_name: &str,
    uri: &str,
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Result<Value, String> {
    call_server_for_policy(
        server_name,
        config,
        workspace,
        policy,
        "resources/read",
        json!({ "uri": uri }),
    )
    .await
}

pub(crate) async fn get_prompt_for_policy(
    server_name: &str,
    name: &str,
    arguments: Value,
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Result<Value, String> {
    call_server_for_policy(
        server_name,
        config,
        workspace,
        policy,
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

fn resolve_server_cwd(
    server: &JsonMcpServerConfig,
    workspace: &Path,
) -> Result<CheckedWorkspacePath, String> {
    match server.cwd.as_deref() {
        Some(cwd) if !cwd.is_empty() => resolve_path_checked(cwd, workspace)
            .map_err(|message| format!("MCP server cwd '{}' is invalid: {message}", cwd)),
        _ => resolve_path_checked(".", workspace)
            .map_err(|message| format!("MCP server cwd is invalid: {message}")),
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

fn workspace_roots_result(
    server_name: &str,
    workspace_root: &CheckedWorkspaceChildRoot,
) -> Result<Value, String> {
    workspace_root.validate().map_err(|error| {
        format!("MCP workspace capability invalidated before roots/list: {error}")
    })?;
    Ok(json!({
        "roots": [
            {
                "uri": path_to_file_uri(workspace_root.path()),
                "name": server_name,
            }
        ]
    }))
}

fn remove_cached_tool_descriptors(cache_key: &str) {
    if let Ok(mut cache) = tool_cache().lock() {
        cache.remove(cache_key);
    }
}

fn remove_cached_http_descriptors(cache_key: &str) {
    remove_cached_tool_descriptors(cache_key);
    if let Ok(mut cache) = resource_cache().lock() {
        cache.remove(cache_key);
    }
    if let Ok(mut cache) = prompt_cache().lock() {
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

async fn refresh_server_caches_for_scope(
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
) -> Result<(), String> {
    let mut cache_keys = Vec::new();
    for (server_name, server) in config
        .mcp_servers
        .iter()
        .filter(|(_, server)| server.enabled)
    {
        cache_keys.push(match policy {
            Some(policy) => cache_key_for_policy(server_name, server, workspace, config, policy)?,
            None => cache_key(server_name, server, workspace, config)?,
        });
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
        let cache_key = match policy {
            Some(policy) => cache_key_for_policy(server_name, server, workspace, config, policy),
            None => cache_key(server_name, server, workspace, config),
        };
        if let Ok(cache_key) = cache_key {
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
    fn validate_workspace(&self) -> Result<(), String> {
        self.workspace_root
            .validate()
            .and_then(|_| self.workspace_child_root.validate())
            .and_then(|_| self.server_cwd.validate())
            .and_then(|_| self.process_cwd.validate())
            .map_err(|error| format!("MCP workspace capability invalidated: {error}"))
    }

    async fn initialize(&mut self) -> Result<(), String> {
        self.validate_workspace()?;
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
                &self.workspace_child_root,
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
        self.validate_workspace()?;
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
        self.validate_workspace()?;
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
                &self.workspace_child_root,
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

        self.validate_workspace()?;
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
        || error.starts_with(MCP_STDIO_TRANSPORT_ERROR_PREFIX)
        || error.contains("failed to spawn")
        || error.contains("missing stdin")
        || error.contains("missing stdout")
        || error.contains("missing stderr")
        || error.contains("invalid Content-Length")
        || error.contains("invalid MCP JSON")
        || error.contains("pipe")
}

fn format_mcp_stdio_transport_error(operation: &str, error: &std::io::Error) -> String {
    format!(
        "{MCP_STDIO_TRANSPORT_ERROR_PREFIX} ({:?}) during {operation}: {error}",
        error.kind()
    )
}

fn is_workspace_capability_error(error: &str) -> bool {
    error.contains("MCP workspace capability invalidated")
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

#[cfg(any(target_os = "linux", target_os = "android"))]
fn apply_mcp_child_root_inheritance(
    command: &mut Command,
    workspace_child_root: &CheckedWorkspaceChildRoot,
) -> Result<(), String> {
    let inherited_fd = workspace_child_root.inherited_fd();
    // SAFETY: the closure runs after fork and before exec. It calls only the
    // async-signal-safe fcntl syscall and changes the child copy of this one
    // checked descriptor; the parent's descriptor remains close-on-exec.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(inherited_fd, libc::F_GETFD);
            if flags == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(inherited_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn apply_mcp_child_root_inheritance(
    _: &mut Command,
    _: &CheckedWorkspaceChildRoot,
) -> Result<(), String> {
    Ok(())
}

async fn spawn_server_session_for_scope(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    cache_namespace: &Path,
    client_capabilities: &McpClientCapabilityPolicy,
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
    let tool_cache_key = cache_key_for_scope(
        server_name,
        server,
        workspace,
        cache_namespace,
        config,
        client_capabilities,
    )?;
    let server_cwd = resolve_server_cwd(server, workspace)?;
    let process_cwd = server_cwd.process_cwd()?;
    let workspace_root = resolve_path_checked(".", workspace)
        .map_err(|error| format!("MCP workspace root is invalid: {error}"))?;
    let workspace_child_root = workspace_root.export_to_child().map_err(|error| {
        format!("MCP workspace root cannot be exported to stdio child: {error}")
    })?;
    let resolved_command = resolve_server_command(&server.command);
    let mut command = Command::new(&resolved_command);
    command
        .args(&server.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .current_dir(process_cwd.path());
    apply_mcp_process_flags(&mut command);
    apply_mcp_child_root_inheritance(&mut command, &workspace_child_root).map_err(|error| {
        format!("MCP workspace root cannot be inherited by stdio child: {error}")
    })?;
    for (key, value) in &server.env {
        command.env(key, resolve_env_placeholder(value));
    }

    let stdout_lines = Arc::new(Mutex::new(Vec::new()));
    let stderr_lines = Arc::new(Mutex::new(Vec::new()));
    process_cwd
        .validate()
        .map_err(|error| format!("MCP workspace capability invalidated before spawn: {error}"))?;
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
        workspace_root,
        workspace_child_root,
        server_cwd,
        process_cwd,
        tool_cache_key,
        client_capabilities: client_capabilities.clone(),
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
    let client_capabilities = client_capabilities_for_server(server_name, workspace);
    get_or_create_server_session_for_scope(
        server_name,
        config,
        workspace,
        workspace,
        &client_capabilities,
    )
    .await
}

async fn get_or_create_server_session_for_policy(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
) -> Result<(String, Arc<AsyncMutex<McpServerSession>>), String> {
    let client_capabilities = policy.client_capabilities_for_server(server_name);
    get_or_create_server_session_for_scope(
        server_name,
        config,
        workspace,
        policy.cache_namespace(workspace),
        &client_capabilities,
    )
    .await
}

async fn get_or_create_server_session_for_scope(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    cache_namespace: &Path,
    client_capabilities: &McpClientCapabilityPolicy,
) -> Result<(String, Arc<AsyncMutex<McpServerSession>>), String> {
    let server = config
        .mcp_servers
        .get(server_name)
        .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
    let key = cache_key_for_scope(
        server_name,
        server,
        workspace,
        cache_namespace,
        config,
        client_capabilities,
    )?;
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
        let validation = {
            let guard = existing.lock().await;
            guard.validate_workspace()
        };
        if let Err(error) = validation {
            remove_cached_server_session(&key, &existing);
            let mut guard = existing.lock().await;
            guard.shutdown().await;
            return Err(error);
        }
        return Ok((key, existing));
    }

    let created = Arc::new(AsyncMutex::new(
        spawn_server_session_for_scope(
            server_name,
            config,
            workspace,
            cache_namespace,
            client_capabilities,
        )
        .await?,
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
        drop(created_guard);
        let validation = {
            let guard = existing.lock().await;
            guard.validate_workspace()
        };
        if let Err(error) = validation {
            remove_cached_server_session(&key, &existing);
            let mut guard = existing.lock().await;
            guard.shutdown().await;
            return Err(error);
        }
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
    if state
        .access_token
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
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

fn http_control_is_current_locked(
    state: &HttpRuntimeState,
    cache_key: &str,
    lease: &HttpControlLease,
    epoch: u64,
) -> bool {
    state
        .controls
        .get(cache_key)
        .is_some_and(|current| Arc::ptr_eq(current, &lease.control))
        && lease.control.epoch.load(Ordering::Acquire) == epoch
}

fn validate_http_one_shot_scope_authority(
    authority: &HttpOneShotScopeAuthority,
) -> Result<(), String> {
    let state = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
    if !http_control_is_current_locked(
        &state,
        &authority.cache_key,
        &authority.control,
        authority.epoch,
    ) {
        return Err("HTTP MCP one-shot scope authority was superseded".to_string());
    }
    if state.cleanups.contains_key(&authority.cache_key) {
        return Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string());
    }
    Ok(())
}

const HTTP_MCP_CLEANUP_UNCERTAIN_ERROR: &str =
    "HTTP MCP remote cleanup is unconfirmed; reinitialization is blocked";
const HTTP_MCP_SESSION_SUPERSEDED_ERROR: &str =
    "HTTP MCP session scope was superseded by another cache key";

fn http_cleanup_blocks_initialize(cache_key: &str) -> bool {
    match http_runtime_state().lock() {
        Ok(state) => state.cleanups.contains_key(cache_key),
        Err(_) => true,
    }
}

fn http_cleanup_is_uncertain(cache_key: &str) -> bool {
    match http_runtime_state().lock() {
        Ok(state) => state
            .cleanups
            .get(cache_key)
            .is_some_and(|cleanup| cleanup.phase == HttpCleanupPhase::Uncertain),
        Err(_) => true,
    }
}

fn http_supersession_is_current_locked(
    state: &HttpRuntimeState,
    supersession: &HttpSessionSupersession,
) -> bool {
    state
        .remote_domain_by_cache_key
        .get(&supersession.replacement_cache_key)
        == Some(&supersession.remote_domain_key)
        && state
            .controls
            .get(&supersession.replacement_cache_key)
            .is_some_and(|control| {
                control.epoch.load(Ordering::Acquire) == supersession.replacement.epoch
            })
        && state
            .sessions
            .get(&supersession.replacement_cache_key)
            .is_some_and(|entry| {
                entry.epoch == supersession.replacement.epoch
                    && entry.generation == supersession.replacement.generation
                    && entry.session_id.as_deref()
                        == Some(supersession.replacement.session_id.as_str())
            })
}

fn reject_or_prune_http_supersession_locked(
    state: &mut HttpRuntimeState,
    cache_key: &str,
) -> Result<(), String> {
    let Some(supersession) = state.supersessions.get(cache_key).cloned() else {
        return Ok(());
    };
    if http_supersession_is_current_locked(state, &supersession) {
        return Err(HTTP_MCP_SESSION_SUPERSEDED_ERROR.to_string());
    }
    state.supersessions.remove(cache_key);
    Ok(())
}

fn remove_http_supersessions_for_replacement_locked(
    state: &mut HttpRuntimeState,
    cache_key: &str,
    identity: Option<&HttpSessionIdentity>,
) {
    state.supersessions.retain(|_, supersession| {
        if supersession.replacement_cache_key != cache_key {
            return true;
        }
        identity.is_some_and(|identity| &supersession.replacement != identity)
    });
}

fn http_remote_cleanup_blocks_cache_key_locked(state: &HttpRuntimeState, cache_key: &str) -> bool {
    let remote_domain = if is_http_remote_cleanup_domain_key(cache_key) {
        Some(cache_key)
    } else {
        state
            .remote_domain_by_cache_key
            .get(cache_key)
            .map(String::as_str)
    };
    remote_domain.is_some_and(|remote_domain| state.cleanups.contains_key(remote_domain))
}

fn http_identity_has_in_flight_requests_locked(
    state: &HttpRuntimeState,
    cache_key: &str,
    identity: &HttpSessionIdentity,
) -> bool {
    state
        .in_flight_requests
        .get(&HttpInFlightRequestKey {
            cache_key: cache_key.to_string(),
            epoch: identity.epoch,
            generation: identity.generation,
        })
        .is_some_and(|count| *count != 0)
}

fn http_session_lookup(cache_key: &str) -> Result<HttpSessionLookup, String> {
    let mut runtime = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
    reject_or_prune_http_supersession_locked(&mut runtime, cache_key)?;
    if http_remote_cleanup_blocks_cache_key_locked(&runtime, cache_key) {
        return Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string());
    }
    let current_epoch = runtime
        .controls
        .get(cache_key)
        .map(|control| control.epoch.load(Ordering::Acquire));
    let Some(entry) = runtime.sessions.get_mut(cache_key) else {
        return Ok(HttpSessionLookup::Missing);
    };
    if current_epoch != Some(entry.epoch) {
        return Err("HTTP MCP session epoch is inconsistent".to_string());
    }
    if entry.last_used_at.elapsed() >= session_idle_ttl() {
        let session_id = entry.session_id.as_ref().ok_or_else(|| {
            "expired HTTP MCP session is missing its remote identifier".to_string()
        })?;
        let identity = HttpSessionIdentity {
            session_id: session_id.clone(),
            epoch: entry.epoch,
            generation: entry.generation,
        };
        if http_identity_has_in_flight_requests_locked(&runtime, cache_key, &identity) {
            let entry = runtime
                .sessions
                .get_mut(cache_key)
                .ok_or_else(|| "HTTP MCP active Session disappeared".to_string())?;
            entry.last_used_at = Instant::now();
            return Ok(HttpSessionLookup::Active(entry.session_id.clone()));
        }
        return Ok(HttpSessionLookup::Expired(identity));
    }
    if let Err(error) = entry.workspace_root.validate() {
        return Err(format!("MCP workspace capability invalidated: {error}"));
    }
    entry.last_used_at = Instant::now();
    Ok(HttpSessionLookup::Active(entry.session_id.clone()))
}

fn cached_http_session_identity_unchecked(cache_key: &str) -> Option<HttpSessionIdentity> {
    let state = http_runtime_state().lock().ok()?;
    let entry = state.sessions.get(cache_key)?;
    (state.controls.get(cache_key)?.epoch.load(Ordering::Acquire) == entry.epoch).then_some(())?;
    Some(HttpSessionIdentity {
        session_id: entry.session_id.clone()?,
        epoch: entry.epoch,
        generation: entry.generation,
    })
}

fn cached_http_session_identity_matching(
    cache_key: &str,
    session_id: Option<&str>,
) -> Option<HttpSessionIdentity> {
    let identity = cached_http_session_identity_unchecked(cache_key)?;
    (Some(identity.session_id.as_str()) == session_id).then_some(identity)
}

fn http_session_identity_is_current(cache_key: &str, identity: &HttpSessionIdentity) -> bool {
    let Ok(state) = http_runtime_state().lock() else {
        return false;
    };
    state
        .controls
        .get(cache_key)
        .is_some_and(|control| control.epoch.load(Ordering::Acquire) == identity.epoch)
        && state.sessions.get(cache_key).is_some_and(|entry| {
            entry.epoch == identity.epoch
                && entry.generation == identity.generation
                && entry.session_id.as_deref() == Some(identity.session_id.as_str())
        })
}

fn capture_http_request_authority(
    cache_key: &str,
    session_id: Option<&str>,
    initialize: bool,
) -> Result<HttpRequestAuthority, String> {
    let control = http_control_lease(cache_key)?;
    let mut state = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
    reject_or_prune_http_supersession_locked(&mut state, cache_key)?;
    let epoch = control.control.epoch.load(Ordering::Acquire);
    if !http_control_is_current_locked(&state, cache_key, &control, epoch) {
        return Err("HTTP MCP request control was superseded".to_string());
    }
    if http_remote_cleanup_blocks_cache_key_locked(&state, cache_key) {
        return Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string());
    }
    let current = state.sessions.get(cache_key);
    if current.is_some_and(|entry| entry.epoch != epoch) {
        return Err("HTTP MCP session epoch is inconsistent".to_string());
    }
    let identity = current.and_then(|entry| {
        let current_session_id = entry.session_id.as_deref()?;
        (Some(current_session_id) == session_id).then(|| HttpSessionIdentity {
            session_id: current_session_id.to_string(),
            epoch,
            generation: entry.generation,
        })
    });
    if initialize {
        if state.cleanups.contains_key(cache_key) {
            return Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string());
        }
        if session_id.is_some() || current.is_some() {
            return Err("HTTP MCP initialize authority was superseded".to_string());
        }
    } else if session_id.is_some() && identity.is_none() {
        return Err("HTTP MCP session changed before the request was sent".to_string());
    }
    let in_flight = if initialize {
        None
    } else {
        identity.as_ref().map(|identity| {
            let key = HttpInFlightRequestKey {
                cache_key: cache_key.to_string(),
                epoch: identity.epoch,
                generation: identity.generation,
            };
            *state.in_flight_requests.entry(key.clone()).or_insert(0) += 1;
            HttpInFlightRequestLease {
                key,
                completed: false,
            }
        })
    };
    Ok(HttpRequestAuthority {
        _in_flight: in_flight,
        control,
        epoch,
        identity,
        descriptor_epoch: state
            .controls
            .get(cache_key)
            .map(|control| control.descriptor_epoch.load(Ordering::Acquire))
            .ok_or_else(|| "HTTP MCP descriptor control was removed".to_string())?,
        initialize,
    })
}

fn http_descriptor_authority_from_request(
    cache_key: &str,
    authority: &HttpRequestAuthority,
    identity: Option<&HttpSessionIdentity>,
) -> HttpDescriptorCacheAuthority {
    HttpDescriptorCacheAuthority {
        cache_key: cache_key.to_string(),
        epoch: authority.epoch,
        identity: identity.cloned(),
        descriptor_epoch: authority.descriptor_epoch,
    }
}

fn capture_http_descriptor_cache_permit(
    cache_key: &str,
) -> Result<HttpDescriptorCachePermit, String> {
    let control = http_control_lease(cache_key)?;
    let mut state = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
    reject_or_prune_http_supersession_locked(&mut state, cache_key)?;
    if http_remote_cleanup_blocks_cache_key_locked(&state, cache_key) {
        return Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string());
    }
    let epoch = control.control.epoch.load(Ordering::Acquire);
    if !http_control_is_current_locked(&state, cache_key, &control, epoch) {
        return Err("HTTP MCP descriptor control was superseded".to_string());
    }
    let identity = state.sessions.get(cache_key).and_then(|entry| {
        let session_id = entry.session_id.as_ref()?;
        (entry.epoch == epoch).then(|| HttpSessionIdentity {
            session_id: session_id.clone(),
            epoch,
            generation: entry.generation,
        })
    });
    Ok(HttpDescriptorCachePermit {
        authority: HttpDescriptorCacheAuthority {
            cache_key: cache_key.to_string(),
            epoch,
            identity,
            descriptor_epoch: control.control.descriptor_epoch.load(Ordering::Acquire),
        },
        _control: control,
    })
}

fn http_descriptor_authority_is_current_locked(
    state: &HttpRuntimeState,
    authority: &HttpDescriptorCacheAuthority,
) -> bool {
    if http_remote_cleanup_blocks_cache_key_locked(state, &authority.cache_key)
        || state
            .supersessions
            .get(&authority.cache_key)
            .is_some_and(|supersession| http_supersession_is_current_locked(state, supersession))
    {
        return false;
    }
    let Some(control) = state.controls.get(&authority.cache_key) else {
        return false;
    };
    if control.epoch.load(Ordering::Acquire) != authority.epoch
        || control.descriptor_epoch.load(Ordering::Acquire) != authority.descriptor_epoch
    {
        return false;
    }
    match authority.identity.as_ref() {
        Some(identity) => state
            .sessions
            .get(&authority.cache_key)
            .is_some_and(|entry| {
                entry.epoch == identity.epoch
                    && entry.generation == identity.generation
                    && entry.session_id.as_deref() == Some(identity.session_id.as_str())
            }),
        None => !state.sessions.contains_key(&authority.cache_key),
    }
}

fn http_descriptor_authority_is_current(authority: &HttpDescriptorCacheAuthority) -> bool {
    http_runtime_state()
        .lock()
        .is_ok_and(|state| http_descriptor_authority_is_current_locked(&state, authority))
}

fn advance_http_descriptor_epoch(cache_key: &str) {
    if let Ok(state) = http_runtime_state().lock()
        && let Some(control) = state.controls.get(cache_key)
    {
        control.descriptor_epoch.fetch_add(1, Ordering::AcqRel);
    }
}

fn http_request_authority_is_current(cache_key: &str, authority: &HttpRequestAuthority) -> bool {
    let Ok(state) = http_runtime_state().lock() else {
        return false;
    };
    if !http_control_is_current_locked(&state, cache_key, &authority.control, authority.epoch) {
        return false;
    }
    if http_remote_cleanup_blocks_cache_key_locked(&state, cache_key) {
        return false;
    }
    match authority.identity.as_ref() {
        Some(identity) => state.sessions.get(cache_key).is_some_and(|entry| {
            entry.epoch == identity.epoch
                && entry.generation == identity.generation
                && entry.session_id.as_deref() == Some(identity.session_id.as_str())
        }),
        None => !state.sessions.contains_key(cache_key),
    }
}

fn http_response_identity_is_current(
    cache_key: &str,
    authority: &HttpRequestAuthority,
    identity: Option<&HttpSessionIdentity>,
) -> bool {
    let Ok(state) = http_runtime_state().lock() else {
        return false;
    };
    if !http_control_is_current_locked(&state, cache_key, &authority.control, authority.epoch) {
        return false;
    }
    if http_remote_cleanup_blocks_cache_key_locked(&state, cache_key) {
        return false;
    }
    match identity {
        Some(identity) => state.sessions.get(cache_key).is_some_and(|entry| {
            entry.epoch == identity.epoch
                && entry.generation == identity.generation
                && entry.session_id.as_deref() == Some(identity.session_id.as_str())
        }),
        None => !state.sessions.contains_key(cache_key),
    }
}

fn apply_http_response_session(
    cache_key: &str,
    session_id: Option<String>,
    workspace_root: &CheckedWorkspacePath,
    authority: &HttpRequestAuthority,
) -> Result<Option<HttpSessionIdentity>, String> {
    let mut state = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
    if !http_control_is_current_locked(&state, cache_key, &authority.control, authority.epoch) {
        return Err("HTTP MCP response belongs to an invalidated request epoch".to_string());
    }
    if authority.initialize {
        if state.cleanups.contains_key(cache_key) {
            return Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string());
        }
        if state.sessions.contains_key(cache_key) {
            return Err("HTTP MCP initialize response was superseded".to_string());
        }
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let remote_domain_key = state
            .remote_domain_by_cache_key
            .get(cache_key)
            .cloned()
            .ok_or_else(|| "HTTP MCP initialize is missing endpoint authority".to_string())?;
        let identity = HttpSessionIdentity {
            session_id: session_id.clone(),
            epoch: authority.epoch,
            generation: next_http_session_generation(),
        };
        let candidate_keys = state
            .sessions
            .iter()
            .filter(|(candidate_key, entry)| {
                candidate_key.as_str() != cache_key
                    && state.remote_domain_by_cache_key.get(*candidate_key)
                        == Some(&remote_domain_key)
                    && entry.session_id.as_deref() == Some(session_id.as_str())
            })
            .map(|(candidate_key, _)| candidate_key.clone())
            .collect::<Vec<_>>();
        let mut candidates = Vec::with_capacity(candidate_keys.len());
        for candidate_key in candidate_keys {
            let entry = state
                .sessions
                .get(&candidate_key)
                .ok_or_else(|| "HTTP MCP replacement Session disappeared".to_string())?;
            let candidate_identity = HttpSessionIdentity {
                session_id: session_id.clone(),
                epoch: entry.epoch,
                generation: entry.generation,
            };
            let control = state.controls.get(&candidate_key).cloned().ok_or_else(|| {
                "HTTP MCP replacement Session is missing its request control".to_string()
            })?;
            if control.epoch.load(Ordering::Acquire) != candidate_identity.epoch {
                ensure_http_cleanup_uncertain_locked(&mut state, &remote_domain_key, true);
                return Err("HTTP MCP replacement Session epoch is inconsistent".to_string());
            }
            candidates.push((candidate_key, candidate_identity, control));
        }

        let mut retirements = Vec::with_capacity(candidates.len());
        for (candidate_key, candidate_identity, control) in &candidates {
            control
                .epoch
                .store(next_http_session_epoch(), Ordering::Release);
            state.sessions.remove(candidate_key);
            state.last_event_ids.remove(candidate_key);
            let stream_task = state
                .stream_tasks
                .remove(candidate_key)
                .map(|entry| entry.handle);
            retirements.push((candidate_key.clone(), control.clone(), stream_task));

            debug_assert_eq!(candidate_identity.session_id, identity.session_id);
        }
        for supersession in state.supersessions.values_mut() {
            if supersession.remote_domain_key == remote_domain_key
                && supersession.replacement.session_id == identity.session_id
            {
                supersession.replacement_cache_key = cache_key.to_string();
                supersession.replacement = identity.clone();
            }
        }
        state.supersessions.remove(cache_key);
        for (candidate_key, _, _) in &retirements {
            state.supersessions.insert(
                candidate_key.clone(),
                HttpSessionSupersession {
                    remote_domain_key: remote_domain_key.clone(),
                    replacement_cache_key: cache_key.to_string(),
                    replacement: identity.clone(),
                },
            );
        }
        state.sessions.insert(
            cache_key.to_string(),
            CachedHttpMcpSession {
                session_id: Some(session_id),
                epoch: identity.epoch,
                generation: identity.generation,
                last_used_at: Instant::now(),
                workspace_root: workspace_root.clone(),
            },
        );
        state.last_event_ids.remove(cache_key);
        drop(state);
        for (candidate_key, control, stream_task) in retirements {
            if let Some(stream_task) = stream_task {
                stream_task.abort();
            }
            remove_cached_http_descriptors(&candidate_key);
            try_reclaim_http_control(&candidate_key, &control);
        }
        return Ok(Some(identity));
    }

    match authority.identity.as_ref() {
        Some(expected) => {
            let current = state
                .sessions
                .get_mut(cache_key)
                .ok_or_else(|| "HTTP MCP response belongs to a removed session".to_string())?;
            if current.epoch != expected.epoch
                || current.generation != expected.generation
                || current.session_id.as_deref() != Some(expected.session_id.as_str())
            {
                return Err("HTTP MCP response belongs to a superseded session".to_string());
            }
            if session_id
                .as_deref()
                .is_some_and(|session_id| session_id != expected.session_id)
            {
                return Err("HTTP MCP response attempted to replace an active session".to_string());
            }
            current.last_used_at = Instant::now();
            Ok(Some(expected.clone()))
        }
        None => {
            if state.sessions.contains_key(cache_key) || session_id.is_some() {
                return Err(
                    "HTTP MCP response attempted to create a session outside initialize"
                        .to_string(),
                );
            }
            Ok(None)
        }
    }
}

#[cfg(test)]
fn set_http_session_id(
    cache_key: &str,
    session_id: Option<String>,
    workspace_root: &CheckedWorkspacePath,
    force_new_generation: bool,
) -> Option<HttpSessionIdentity> {
    assert!(
        force_new_generation,
        "tests must install a fresh generation"
    );
    let session_id = session_id?;
    let mut state = http_runtime_state().lock().ok()?;
    let control = state
        .controls
        .entry(cache_key.to_string())
        .or_insert_with(|| Arc::new(HttpSessionControl::new()))
        .clone();
    let epoch = next_http_session_epoch();
    control.epoch.store(epoch, Ordering::Release);
    let identity = HttpSessionIdentity {
        session_id: session_id.clone(),
        epoch,
        generation: next_http_session_generation(),
    };
    state.sessions.insert(
        cache_key.to_string(),
        CachedHttpMcpSession {
            session_id: Some(session_id),
            epoch: identity.epoch,
            generation: identity.generation,
            last_used_at: Instant::now(),
            workspace_root: workspace_root.clone(),
        },
    );
    state.last_event_ids.remove(cache_key);
    Some(identity)
}

fn remove_http_session_state_if_generation(
    cache_key: &str,
    identity: &HttpSessionIdentity,
) -> bool {
    let Ok(mut state) = http_runtime_state().lock() else {
        return false;
    };
    let Some(control) = state.controls.get(cache_key).cloned() else {
        return false;
    };
    if control.epoch.load(Ordering::Acquire) != identity.epoch
        || !state.sessions.get(cache_key).is_some_and(|entry| {
            entry.epoch == identity.epoch
                && entry.generation == identity.generation
                && entry.session_id.as_deref() == Some(identity.session_id.as_str())
        })
    {
        return false;
    }
    control
        .epoch
        .store(next_http_session_epoch(), Ordering::Release);
    state.sessions.remove(cache_key);
    remove_http_supersessions_for_replacement_locked(&mut state, cache_key, Some(identity));
    if state.last_event_ids.get(cache_key).is_some_and(|entry| {
        entry.epoch == identity.epoch && entry.generation == identity.generation
    }) {
        state.last_event_ids.remove(cache_key);
    }
    let removed_task = if state.stream_tasks.get(cache_key).is_some_and(|entry| {
        entry.epoch == identity.epoch && entry.generation == identity.generation
    }) {
        state
            .stream_tasks
            .remove(cache_key)
            .map(|entry| entry.handle)
    } else {
        None
    };
    drop(state);
    if let Some(handle) = removed_task {
        handle.abort();
    }
    remove_cached_http_descriptors(cache_key);
    try_reclaim_http_control(cache_key, &control);
    true
}

fn remove_http_session_if_generation(cache_key: &str, identity: &HttpSessionIdentity) {
    remove_http_session_state_if_generation(cache_key, identity);
}

fn fail_closed_http_timeout_before_cancel(
    cache_key: &str,
    server: &JsonMcpServerConfig,
    identity: Option<&HttpSessionIdentity>,
    one_shot_scope: Option<&HttpOneShotScopeAuthority>,
    request_authority: &mut HttpRequestAuthority,
) {
    let remote_domain = match one_shot_scope {
        Some(authority) => Some(authority.cache_key.clone()),
        None if identity.is_some() => http_remote_domain_for_cache_key(cache_key, server).ok(),
        None => None,
    };
    let Some(remote_domain) = remote_domain else {
        return;
    };
    // The cancellation notification performs its own token lookup and network
    // request. Make the stable endpoint quarantine visible before any such
    // await for ordinary and one-shot requests, including initialize attempts
    // that do not have a remote Session identity yet. The one-shot owner may
    // later observe or attempt cleanup, but sticky uncertainty must remain the
    // authorization boundary for that endpoint.
    ensure_http_cleanup_uncertain(&remote_domain);
    if let Some(identity) = identity {
        remove_http_session_if_generation(cache_key, identity);
    }
    request_authority.abandon_in_flight();
}

fn remove_http_session(cache_key: &str) {
    let (control, removed_task) = match http_runtime_state().lock() {
        Ok(mut state) => {
            let control = state
                .controls
                .entry(cache_key.to_string())
                .or_insert_with(|| Arc::new(HttpSessionControl::new()))
                .clone();
            if is_http_remote_cleanup_domain_key(cache_key)
                && control.leases.load(Ordering::Acquire) != 0
            {
                ensure_http_cleanup_uncertain_locked(&mut state, cache_key, false);
            }
            control
                .epoch
                .store(next_http_session_epoch(), Ordering::Release);
            state.sessions.remove(cache_key);
            state.last_event_ids.remove(cache_key);
            remove_http_supersessions_for_replacement_locked(&mut state, cache_key, None);
            let task = state
                .stream_tasks
                .remove(cache_key)
                .map(|entry| entry.handle);
            (Some(control), task)
        }
        Err(_) => (None, None),
    };
    if let Some(handle) = removed_task {
        handle.abort();
    }
    remove_cached_http_descriptors(cache_key);
    if let Some(control) = control {
        try_reclaim_http_control(cache_key, &control);
    }
}

fn cache_key_belongs_to_server(cache_key: &str, server_name: &str) -> bool {
    cache_key.lines().next() == Some(server_name)
}

fn cached_http_session_keys_for_server(server_name: &str) -> Vec<String> {
    let mut keys = HashSet::new();
    if let Ok(state) = http_runtime_state().lock() {
        keys.extend(
            state
                .controls
                .keys()
                .filter(|key| cache_key_belongs_to_server(key, server_name))
                .cloned(),
        );
        keys.extend(
            state
                .sessions
                .keys()
                .filter(|key| cache_key_belongs_to_server(key, server_name))
                .cloned(),
        );
        keys.extend(
            state
                .stream_tasks
                .keys()
                .filter(|key| cache_key_belongs_to_server(key, server_name))
                .cloned(),
        );
        keys.extend(
            state
                .last_event_ids
                .keys()
                .filter(|key| cache_key_belongs_to_server(key, server_name))
                .cloned(),
        );
        keys.extend(
            state
                .cleanups
                .keys()
                .filter(|key| cache_key_belongs_to_server(key, server_name))
                .cloned(),
        );
        keys.extend(
            state
                .cleanup_tasks
                .keys()
                .filter(|key| cache_key_belongs_to_server(key, server_name))
                .cloned(),
        );
        keys.extend(
            state
                .supersessions
                .keys()
                .filter(|key| cache_key_belongs_to_server(key, server_name))
                .cloned(),
        );
        if let Some(domains) = state.remote_domains_by_server.get(server_name) {
            keys.extend(domains.iter().cloned());
        }
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
        if is_http_remote_cleanup_domain_key(&cache_key) {
            continue;
        }
        terminate_http_session(server_name, &cache_key, server).await;
    }
}

pub(crate) fn clear_cached_runtime_state_for_server(server_name: &str) {
    clear_descriptor_caches_for_server(server_name);
    for cache_key in cached_http_session_keys_for_server(server_name) {
        remove_http_session(&cache_key);
    }
}

fn http_last_event_id(cache_key: &str, identity: &HttpSessionIdentity) -> Option<String> {
    let state = http_runtime_state().lock().ok()?;
    if state.controls.get(cache_key)?.epoch.load(Ordering::Acquire) != identity.epoch {
        return None;
    }
    let current = state.sessions.get(cache_key)?;
    if current.epoch != identity.epoch
        || current.generation != identity.generation
        || current.session_id.as_deref() != Some(identity.session_id.as_str())
    {
        return None;
    }
    state
        .last_event_ids
        .get(cache_key)
        .filter(|entry| entry.epoch == identity.epoch && entry.generation == identity.generation)
        .map(|entry| entry.event_id.clone())
}

fn set_http_last_event_id(cache_key: &str, identity: &HttpSessionIdentity, event_id: &str) {
    let Ok(mut state) = http_runtime_state().lock() else {
        return;
    };
    if state
        .controls
        .get(cache_key)
        .is_none_or(|control| control.epoch.load(Ordering::Acquire) != identity.epoch)
    {
        return;
    }
    let matches = state.sessions.get(cache_key).is_some_and(|entry| {
        entry.epoch == identity.epoch
            && entry.generation == identity.generation
            && entry.session_id.as_deref() == Some(identity.session_id.as_str())
    });
    if matches {
        state.last_event_ids.insert(
            cache_key.to_string(),
            CachedHttpEventId {
                epoch: identity.epoch,
                generation: identity.generation,
                event_id: event_id.to_string(),
            },
        );
    }
}

struct HttpStreamTaskCleanup {
    cache_key: String,
    task_id: u64,
    epoch: u64,
    generation: u64,
}

impl Drop for HttpStreamTaskCleanup {
    fn drop(&mut self) {
        if let Ok(mut state) = http_runtime_state().lock() {
            let should_remove = state
                .stream_tasks
                .get(&self.cache_key)
                .is_some_and(|entry| {
                    entry.task_id == self.task_id
                        && entry.epoch == self.epoch
                        && entry.generation == self.generation
                });
            if should_remove {
                state.stream_tasks.remove(&self.cache_key);
            }
        }
    }
}

fn invalidate_http_server_caches(message: &Value, cache_key: &str) {
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return;
    };
    if matches!(
        method,
        "notifications/tools/list_changed"
            | "notifications/resources/list_changed"
            | "notifications/prompts/list_changed"
    ) {
        advance_http_descriptor_epoch(cache_key);
    }
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

fn record_sse_event_id(event: &str, cache_key: &str, identity: Option<&HttpSessionIdentity>) {
    let Some(identity) = identity else {
        return;
    };
    for line in event.lines() {
        if let Some(event_id) = line.strip_prefix("id:") {
            let event_id = event_id.trim();
            if !event_id.is_empty() {
                set_http_last_event_id(cache_key, identity, event_id);
            }
        }
    }
}

#[cfg(test)]
fn parse_sse_event(event: &str, cache_key: &str) -> Option<Value> {
    let identity = cached_http_session_identity_unchecked(cache_key);
    record_sse_event_id(event, cache_key, identity.as_ref());
    let message = parse_sse_event_message(event)?;
    if message.get("method").is_some() {
        handle_http_server_message(&message, cache_key);
    }
    Some(message)
}

fn http_server_request_response(
    message: &Value,
    _server_name: &str,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
) -> Result<Option<Value>, String> {
    workspace_root.validate().map_err(|error| {
        format!("MCP workspace capability invalidated before roots response: {error}")
    })?;
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        return Ok(None);
    };
    let Some(id) = message.get("id") else {
        return Ok(None);
    };
    let response = match method {
        "ping" => json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "result": {}
        }),
        "roots/list" => json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "error": {
                "code": -32601,
                "message": "Method not supported: roots/list"
            }
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
    Ok(Some(response))
}

async fn send_http_jsonrpc_message(
    server_name: &str,
    server: &JsonMcpServerConfig,
    session_id: Option<&str>,
    payload: Value,
    timeout_secs: u64,
) -> Result<(), String> {
    let url = streamable_http_endpoint_url(server)?;
    let client = streamable_http_client_with_timeout(timeout_secs)?;
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

#[allow(clippy::too_many_arguments)]
async fn handle_http_server_message_async(
    message: &Value,
    cache_key: &str,
    server_name: &str,
    server: &JsonMcpServerConfig,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
    session_id: Option<&str>,
    timeout_secs: u64,
) -> Result<(), String> {
    handle_http_server_message_async_with_before_send_hook(
        message,
        cache_key,
        server_name,
        server,
        workspace_root,
        client_capabilities,
        session_id,
        timeout_secs,
        &mut || {},
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn handle_http_server_message_async_with_before_send_hook(
    message: &Value,
    cache_key: &str,
    server_name: &str,
    server: &JsonMcpServerConfig,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
    session_id: Option<&str>,
    timeout_secs: u64,
    before_send: &mut (dyn FnMut() + Send),
) -> Result<(), String> {
    invalidate_http_server_caches(message, cache_key);
    let Some(response) =
        http_server_request_response(message, server_name, workspace_root, client_capabilities)?
    else {
        return Ok(());
    };
    before_send();
    send_http_jsonrpc_message(server_name, server, session_id, response, timeout_secs).await
}

async fn start_http_event_stream(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    session_id: &str,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
    timeout_secs: u64,
) {
    let Some(identity) = cached_http_session_identity_matching(cache_key, Some(session_id)) else {
        return;
    };
    let control_lease = match http_control_lease(cache_key) {
        Ok(lease) => lease,
        Err(_) => return,
    };
    let mut state = match http_runtime_state().lock() {
        Ok(state) => state,
        Err(_) => return,
    };
    if !http_control_is_current_locked(&state, cache_key, &control_lease, identity.epoch)
        || !state.sessions.get(cache_key).is_some_and(|entry| {
            entry.epoch == identity.epoch
                && entry.generation == identity.generation
                && entry.session_id.as_deref() == Some(identity.session_id.as_str())
        })
    {
        return;
    }
    if state.stream_tasks.get(cache_key).is_some_and(|entry| {
        entry.epoch == identity.epoch && entry.generation == identity.generation
    }) {
        return;
    }
    if let Some(old) = state.stream_tasks.remove(cache_key) {
        old.handle.abort();
    }

    let server_name = server_name.to_string();
    let server = server.clone();
    let cache_key = cache_key.to_string();
    let task_cache_key = cache_key.clone();
    let workspace_root = workspace_root.clone();
    let client_capabilities = effective_http_client_capabilities(client_capabilities);
    let task_id = next_http_stream_task_id();
    let task_epoch = identity.epoch;
    let task_generation = identity.generation;
    let handle = tokio::spawn(async move {
        let _control_lease = control_lease;
        tokio::task::yield_now().await;
        let _cleanup = HttpStreamTaskCleanup {
            cache_key: task_cache_key.clone(),
            task_id,
            epoch: identity.epoch,
            generation: identity.generation,
        };
        if workspace_root.validate().is_err()
            || !http_session_identity_is_current(&task_cache_key, &identity)
        {
            terminate_stale_http_identity_session(
                &server_name,
                &server,
                &task_cache_key,
                &identity,
                &_control_lease,
            )
            .await;
            remove_http_session_state_if_generation(&task_cache_key, &identity);
            return;
        }
        let Ok(url) = streamable_http_endpoint_url(&server) else {
            return;
        };
        let Ok(client) = streamable_http_client_with_timeout(timeout_secs) else {
            return;
        };
        let mut request = client
            .get(url)
            .header("accept", "text/event-stream")
            .header("mcp-protocol-version", MCP_PROTOCOL_VERSION)
            .header("mcp-session-id", &identity.session_id);
        if let Some(last_event_id) = http_last_event_id(&task_cache_key, &identity) {
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
            remove_http_session_state_if_generation(&task_cache_key, &identity);
            return;
        }
        if !response.status().is_success() {
            return;
        }
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        while let Some(chunk) = stream.next().await {
            if workspace_root.validate().is_err()
                || !http_session_identity_is_current(&task_cache_key, &identity)
            {
                terminate_stale_http_identity_session(
                    &server_name,
                    &server,
                    &task_cache_key,
                    &identity,
                    &_control_lease,
                )
                .await;
                remove_http_session_state_if_generation(&task_cache_key, &identity);
                break;
            }
            let Ok(chunk) = chunk else {
                break;
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            for event in parse_sse_events_from_buffer(&mut buffer) {
                record_sse_event_id(&event, &task_cache_key, Some(&identity));
                if let Some(message) = parse_sse_event_message(&event)
                    && handle_http_server_message_async(
                        &message,
                        &task_cache_key,
                        &server_name,
                        &server,
                        &workspace_root,
                        &client_capabilities,
                        Some(&identity.session_id),
                        timeout_secs,
                    )
                    .await
                    .is_err()
                {
                    terminate_stale_http_identity_session(
                        &server_name,
                        &server,
                        &task_cache_key,
                        &identity,
                        &_control_lease,
                    )
                    .await;
                    remove_http_session_state_if_generation(&task_cache_key, &identity);
                    return;
                }
            }
        }
        if !buffer.trim().is_empty() {
            record_sse_event_id(&buffer, &task_cache_key, Some(&identity));
            if let Some(message) = parse_sse_event_message(&buffer)
                && handle_http_server_message_async(
                    &message,
                    &task_cache_key,
                    &server_name,
                    &server,
                    &workspace_root,
                    &client_capabilities,
                    Some(&identity.session_id),
                    timeout_secs,
                )
                .await
                .is_err()
            {
                terminate_stale_http_identity_session(
                    &server_name,
                    &server,
                    &task_cache_key,
                    &identity,
                    &_control_lease,
                )
                .await;
                remove_http_session_state_if_generation(&task_cache_key, &identity);
            }
        }
    });
    state.stream_tasks.insert(
        cache_key,
        HttpStreamTaskEntry {
            task_id,
            epoch: task_epoch,
            generation: task_generation,
            handle,
        },
    );
}

async fn send_http_cancelled_notification(
    server_name: &str,
    server: &JsonMcpServerConfig,
    session_id: Option<&str>,
    request_id: Value,
    reason: &str,
) {
    let Ok(url) = streamable_http_endpoint_url(server) else {
        return;
    };
    let Ok(client) = streamable_http_client_with_timeout(2) else {
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

enum HttpCleanupStart {
    SafeWithoutDelete,
    AlreadyBlocked,
    Started {
        cleanup_id: u64,
        removed_stream_task: Option<JoinHandle<()>>,
        removed_local_session: bool,
        deferred_wait: Option<HttpDeferredCleanupWait>,
    },
}

enum HttpRemoteCleanupStart {
    Started(HttpCleanupQuarantineObserver),
    SafeWithoutDelete,
}

struct HttpCleanupTaskCleanup {
    cache_key: String,
    cleanup_id: u64,
    deferred_key: Option<HttpInFlightRequestKey>,
}

impl Drop for HttpCleanupTaskCleanup {
    fn drop(&mut self) {
        let Ok(mut state) = http_runtime_state().lock() else {
            return;
        };
        if state
            .cleanup_tasks
            .get(&self.cache_key)
            .is_some_and(|entry| entry.cleanup_id == self.cleanup_id)
        {
            state.cleanup_tasks.remove(&self.cache_key);
        }
        if let Some(deferred_key) = self.deferred_key.as_ref()
            && state
                .deferred_cleanups
                .get(deferred_key)
                .is_some_and(|cleanup| cleanup.cleanup_id == self.cleanup_id)
        {
            state.deferred_cleanups.remove(deferred_key);
        }
        if let Some(cleanup) = state.cleanups.get_mut(&self.cache_key)
            && cleanup.cleanup_id == self.cleanup_id
        {
            cleanup.phase = HttpCleanupPhase::Uncertain;
            cleanup.sticky_uncertainty = true;
        }
    }
}

struct HttpCleanupWaitGuard {
    cache_key: String,
    cleanup_id: u64,
    completed: bool,
}

impl Drop for HttpCleanupWaitGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        mark_http_cleanup_uncertain(&self.cache_key, self.cleanup_id, false);
    }
}

fn mark_http_cleanup_uncertain(cache_key: &str, cleanup_id: u64, sticky: bool) {
    let Ok(mut state) = http_runtime_state().lock() else {
        return;
    };
    mark_http_cleanup_uncertain_locked(&mut state, cache_key, cleanup_id, sticky);
}

fn mark_http_cleanup_uncertain_locked(
    state: &mut HttpRuntimeState,
    cache_key: &str,
    cleanup_id: u64,
    sticky: bool,
) {
    if let Some(cleanup) = state.cleanups.get_mut(cache_key)
        && cleanup.cleanup_id == cleanup_id
    {
        cleanup.phase = HttpCleanupPhase::Uncertain;
        cleanup.sticky_uncertainty |= sticky;
    }
}

fn ensure_http_cleanup_uncertain_locked(
    state: &mut HttpRuntimeState,
    cache_key: &str,
    sticky: bool,
) -> u64 {
    if let Some(cleanup) = state.cleanups.get_mut(cache_key) {
        // A second independent uncertainty that overlaps a Pending DELETE is
        // process-sticky even when its own source could otherwise be resolved
        // by a later known-session cleanup. This prevents the older observer
        // from authorizing a replacement generation.
        let overlapped_pending = cleanup.phase == HttpCleanupPhase::Pending;
        cleanup.phase = HttpCleanupPhase::Uncertain;
        cleanup.sticky_uncertainty |= sticky || overlapped_pending;
        return cleanup.cleanup_id;
    }
    let cleanup_id = next_http_cleanup_id();
    state.cleanups.insert(
        cache_key.to_string(),
        HttpCleanupState {
            cleanup_id,
            phase: HttpCleanupPhase::Uncertain,
            sticky_uncertainty: sticky,
            owner_cache_key: None,
            owner_identity: None,
        },
    );
    cleanup_id
}

fn ensure_http_cleanup_uncertain(cache_key: &str) {
    if let Ok(mut state) = http_runtime_state().lock() {
        ensure_http_cleanup_uncertain_locked(&mut state, cache_key, true);
    }
}

fn fail_closed_http_initialize_attempt(
    remote_domain_key: &str,
    installed_session: Option<&(String, HttpSessionIdentity)>,
) {
    let (control, stream_task, removed_cache_key) = match http_runtime_state().lock() {
        Ok(mut state) => {
            let runtime_cleanup_owns_session =
                installed_session.is_some_and(|(installed_cache_key, installed_identity)| {
                    state
                        .cleanups
                        .get(remote_domain_key)
                        .is_some_and(|cleanup| {
                            cleanup.phase == HttpCleanupPhase::Pending
                                && cleanup.owner_cache_key.as_deref()
                                    == Some(installed_cache_key.as_str())
                                && cleanup.owner_identity.as_ref() == Some(installed_identity)
                        })
                });
            if !runtime_cleanup_owns_session {
                ensure_http_cleanup_uncertain_locked(&mut state, remote_domain_key, true);
            }
            let Some((cache_key, identity)) = installed_session else {
                return;
            };
            let Some(control) = state.controls.get(cache_key).cloned() else {
                return;
            };
            let is_current = control.epoch.load(Ordering::Acquire) == identity.epoch
                && state.sessions.get(cache_key).is_some_and(|entry| {
                    entry.epoch == identity.epoch
                        && entry.generation == identity.generation
                        && entry.session_id.as_deref() == Some(identity.session_id.as_str())
                });
            if !is_current {
                return;
            }
            control
                .epoch
                .store(next_http_session_epoch(), Ordering::Release);
            control.descriptor_epoch.fetch_add(1, Ordering::AcqRel);
            state.sessions.remove(cache_key);
            remove_http_supersessions_for_replacement_locked(&mut state, cache_key, Some(identity));
            if state.last_event_ids.get(cache_key).is_some_and(|entry| {
                entry.epoch == identity.epoch && entry.generation == identity.generation
            }) {
                state.last_event_ids.remove(cache_key);
            }
            let stream_task = if state.stream_tasks.get(cache_key).is_some_and(|entry| {
                entry.epoch == identity.epoch && entry.generation == identity.generation
            }) {
                state
                    .stream_tasks
                    .remove(cache_key)
                    .map(|entry| entry.handle)
            } else {
                None
            };
            (Some(control), stream_task, Some(cache_key.clone()))
        }
        Err(_) => (None, None, None),
    };
    if let Some(stream_task) = stream_task {
        stream_task.abort();
    }
    if let Some(cache_key) = removed_cache_key.as_deref() {
        remove_cached_http_descriptors(cache_key);
    }
    if let (Some(cache_key), Some(control)) = (removed_cache_key.as_deref(), control.as_ref()) {
        try_reclaim_http_control(cache_key, control);
    }
}

impl HttpOneShotInitializeAttempt {
    fn new(authority: &HttpOneShotScopeAuthority) -> Self {
        Self {
            cache_key: authority.cache_key.clone(),
            _control: authority.control.clone(),
            installed_session: None,
            may_have_been_sent: false,
            observed_cleanup: None,
            completed: false,
        }
    }

    fn mark_may_have_been_sent(&mut self) {
        self.may_have_been_sent = true;
    }

    fn bind_installed_session(&mut self, cache_key: &str, identity: &HttpSessionIdentity) {
        self.installed_session = Some((cache_key.to_string(), identity.clone()));
    }

    fn complete_handoff(&mut self) {
        self.completed = true;
    }

    fn record_cleanup(&mut self, outcome: HttpDeleteOutcome) {
        self.observed_cleanup = Some(outcome);
    }

    fn into_lifecycle_owner(mut self, cache_key: &str) -> TemporaryHttpLifecycleOwner {
        self.completed = true;
        TemporaryHttpLifecycleOwner {
            cache_key: cache_key.to_string(),
            remote_domain_key: self.cache_key.clone(),
            _control: self._control.clone(),
            installed_session: self
                .installed_session
                .as_ref()
                .map(|(_, identity)| identity.clone()),
            completed: false,
        }
    }

    fn complete_cleanup(&mut self, outcome: HttpDeleteOutcome) {
        let outcome = self.observed_cleanup.unwrap_or(outcome);
        if self.may_have_been_sent && outcome == HttpDeleteOutcome::Ambiguous {
            fail_closed_http_initialize_attempt(&self.cache_key, self.installed_session.as_ref());
        }
        self.completed = true;
    }
}

impl TemporaryHttpLifecycleOwner {
    fn complete_cleanup(&mut self, outcome: HttpDeleteOutcome) {
        if outcome == HttpDeleteOutcome::Ambiguous {
            ensure_http_cleanup_uncertain(&self.remote_domain_key);
        }
        if let Some(identity) = self.installed_session.as_ref() {
            remove_http_session_if_generation(&self.cache_key, identity);
        }
        self.completed = true;
    }
}

impl Drop for TemporaryHttpLifecycleOwner {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        let installed_session = self
            .installed_session
            .as_ref()
            .map(|identity| (self.cache_key.clone(), identity.clone()));
        fail_closed_http_initialize_attempt(&self.remote_domain_key, installed_session.as_ref());
    }
}

impl Drop for HttpOneShotInitializeAttempt {
    fn drop(&mut self) {
        if self.may_have_been_sent && !self.completed {
            fail_closed_http_initialize_attempt(&self.cache_key, self.installed_session.as_ref());
        }
    }
}

impl HttpCleanupQuarantineObserver {
    fn finish(mut self, outcome: HttpDeleteOutcome) {
        finish_http_cleanup(&self.cache_key, self.cleanup_id, outcome);
        self.completed = true;
    }
}

impl Drop for HttpCleanupQuarantineObserver {
    fn drop(&mut self) {
        if !self.completed {
            mark_http_cleanup_uncertain(&self.cache_key, self.cleanup_id, true);
        }
    }
}

fn finish_http_cleanup(cache_key: &str, cleanup_id: u64, outcome: HttpDeleteOutcome) {
    let Ok(mut state) = http_runtime_state().lock() else {
        return;
    };
    let remove_cleanup = match state.cleanups.get_mut(cache_key) {
        Some(cleanup) if cleanup.cleanup_id == cleanup_id => match outcome {
            HttpDeleteOutcome::Confirmed | HttpDeleteOutcome::NotApplied => {
                !cleanup.sticky_uncertainty
            }
            HttpDeleteOutcome::Ambiguous => {
                cleanup.phase = HttpCleanupPhase::Uncertain;
                cleanup.sticky_uncertainty = true;
                false
            }
        },
        _ => return,
    };
    if remove_cleanup {
        state.cleanups.remove(cache_key);
    }
    if state
        .cleanup_tasks
        .get(cache_key)
        .is_some_and(|entry| entry.cleanup_id == cleanup_id)
    {
        state.cleanup_tasks.remove(cache_key);
    }
    state
        .deferred_cleanups
        .retain(|key, cleanup| key.cache_key != cache_key || cleanup.cleanup_id != cleanup_id);
}

fn begin_http_cleanup_quarantine(
    authority: &HttpOneShotScopeAuthority,
    cache_key: &str,
    session_id: &str,
    authorized_identity: Option<&HttpSessionIdentity>,
) -> Result<HttpRemoteCleanupStart, String> {
    let mut state = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
    if !state
        .controls
        .get(&authority.cache_key)
        .is_some_and(|control| Arc::ptr_eq(control, &authority.control.control))
    {
        ensure_http_cleanup_uncertain_locked(&mut state, &authority.cache_key, true);
        return Err("HTTP MCP one-shot cleanup authority was superseded".to_string());
    }
    if let Some(cleanup) = state.cleanups.get(&authority.cache_key)
        && (cleanup.phase == HttpCleanupPhase::Pending || cleanup.sticky_uncertainty)
    {
        return Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string());
    }
    let replacement_owns_same_id = state.sessions.iter().any(|(candidate_key, entry)| {
        if state.remote_domain_by_cache_key.get(candidate_key) != Some(&authority.cache_key)
            || entry.session_id.as_deref() != Some(session_id)
        {
            return false;
        }
        let candidate_is_authorized_source = candidate_key == cache_key
            && authorized_identity.is_some_and(|identity| {
                entry.epoch == identity.epoch
                    && entry.generation == identity.generation
                    && entry.session_id.as_deref() == Some(identity.session_id.as_str())
            });
        !candidate_is_authorized_source
    });
    if replacement_owns_same_id {
        return Ok(HttpRemoteCleanupStart::SafeWithoutDelete);
    }
    let cleanup_id = next_http_cleanup_id();
    state.cleanups.insert(
        authority.cache_key.clone(),
        HttpCleanupState {
            cleanup_id,
            phase: HttpCleanupPhase::Pending,
            sticky_uncertainty: false,
            owner_cache_key: Some(cache_key.to_string()),
            owner_identity: authorized_identity.cloned(),
        },
    );
    Ok(HttpRemoteCleanupStart::Started(
        HttpCleanupQuarantineObserver {
            cache_key: authority.cache_key.clone(),
            cleanup_id,
            _control: authority.control.clone(),
            completed: false,
        },
    ))
}

fn begin_http_cleanup(
    cache_key: &str,
    session_id: &str,
    authorized_identity: Option<&HttpSessionIdentity>,
    guard: &HttpKeyExclusiveGuard,
    remote_cleanup: Option<(&str, u64)>,
) -> Result<HttpCleanupStart, String> {
    let mut state = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
    if !state
        .controls
        .get(cache_key)
        .is_some_and(|control| Arc::ptr_eq(control, &guard.lease.control))
    {
        return Err("HTTP MCP cleanup control was superseded".to_string());
    }
    if state.cleanups.contains_key(cache_key) {
        return Ok(HttpCleanupStart::AlreadyBlocked);
    }

    let current = state.sessions.get(cache_key).and_then(|entry| {
        let session_id = entry.session_id.as_ref()?;
        Some(HttpSessionIdentity {
            session_id: session_id.clone(),
            epoch: entry.epoch,
            generation: entry.generation,
        })
    });
    let authorized_session_is_current =
        authorized_identity.is_some_and(|identity| current.as_ref() == Some(identity));
    let replacement_owns_same_id = current.is_some_and(|identity| {
        identity.session_id == session_id
            && authorized_identity.is_none_or(|authorized| &identity != authorized)
    });
    if !authorized_session_is_current && replacement_owns_same_id {
        return Ok(HttpCleanupStart::SafeWithoutDelete);
    }
    let deferred_key = authorized_session_is_current
        .then_some(authorized_identity)
        .flatten()
        .filter(|identity| http_identity_has_in_flight_requests_locked(&state, cache_key, identity))
        .map(|identity| HttpInFlightRequestKey {
            cache_key: cache_key.to_string(),
            epoch: identity.epoch,
            generation: identity.generation,
        });
    if deferred_key
        .as_ref()
        .is_some_and(|key| state.deferred_cleanups.contains_key(key))
    {
        return Err("HTTP MCP deferred cleanup already exists".to_string());
    }

    let cleanup_id = next_http_cleanup_id();
    state.cleanups.insert(
        cache_key.to_string(),
        HttpCleanupState {
            cleanup_id,
            phase: HttpCleanupPhase::Pending,
            sticky_uncertainty: false,
            owner_cache_key: None,
            owner_identity: None,
        },
    );
    let removed_stream_task = if authorized_session_is_current {
        guard
            .lease
            .control
            .epoch
            .store(next_http_session_epoch(), Ordering::Release);
        state.sessions.remove(cache_key);
        remove_http_supersessions_for_replacement_locked(
            &mut state,
            cache_key,
            authorized_identity,
        );
        if state.last_event_ids.get(cache_key).is_some_and(|entry| {
            authorized_identity.is_some_and(|identity| {
                entry.epoch == identity.epoch && entry.generation == identity.generation
            })
        }) {
            state.last_event_ids.remove(cache_key);
        }
        if state.stream_tasks.get(cache_key).is_some_and(|entry| {
            authorized_identity.is_some_and(|identity| {
                entry.epoch == identity.epoch && entry.generation == identity.generation
            })
        }) {
            state
                .stream_tasks
                .remove(cache_key)
                .map(|entry| entry.handle)
        } else {
            None
        }
    } else {
        None
    };
    let deferred_wait = deferred_key.map(|key| {
        let notify = Arc::new(Notify::new());
        state.deferred_cleanups.insert(
            key.clone(),
            HttpDeferredCleanupState {
                cleanup_id,
                notify: notify.clone(),
                remote_cleanup: remote_cleanup
                    .map(|(cache_key, cleanup_id)| (cache_key.to_string(), cleanup_id)),
                force_uncertain: false,
            },
        );
        HttpDeferredCleanupWait {
            key,
            cleanup_id,
            notify,
        }
    });
    Ok(HttpCleanupStart::Started {
        cleanup_id,
        removed_stream_task,
        removed_local_session: authorized_session_is_current,
        deferred_wait,
    })
}

async fn wait_for_deferred_http_cleanup(wait: &HttpDeferredCleanupWait) -> Result<bool, String> {
    loop {
        // Create the notified future before checking the count so a final Drop
        // between the check and await leaves a permit instead of a lost wakeup.
        let notified = wait.notify.notified();
        let ready = {
            let mut state = http_runtime_state()
                .lock()
                .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
            let Some(cleanup) = state.deferred_cleanups.get(&wait.key) else {
                return Err("HTTP MCP deferred cleanup authority disappeared".to_string());
            };
            if cleanup.cleanup_id != wait.cleanup_id {
                return Err("HTTP MCP deferred cleanup was superseded".to_string());
            }
            if state.in_flight_requests.contains_key(&wait.key) {
                None
            } else {
                let force_uncertain = cleanup.force_uncertain;
                state.deferred_cleanups.remove(&wait.key);
                Some(force_uncertain)
            }
        };
        if let Some(force_uncertain) = ready {
            return Ok(force_uncertain);
        }
        notified.await;
    }
}

fn http_remote_replacement_owns_session_id(
    remote_domain_key: &str,
    source_cache_key: &str,
    authorized_identity: Option<&HttpSessionIdentity>,
    session_id: &str,
) -> Result<bool, String> {
    let state = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP MCP runtime state lock poisoned".to_string())?;
    Ok(state.sessions.iter().any(|(candidate_key, entry)| {
        if state
            .remote_domain_by_cache_key
            .get(candidate_key)
            .map(String::as_str)
            != Some(remote_domain_key)
            || entry.session_id.as_deref() != Some(session_id)
        {
            return false;
        }
        candidate_key != source_cache_key
            || authorized_identity.is_none_or(|identity| {
                entry.epoch != identity.epoch || entry.generation != identity.generation
            })
    }))
}

async fn terminate_http_session_with_guard(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    session_id: Option<&str>,
    authorized_identity: Option<&HttpSessionIdentity>,
    guard: HttpKeyExclusiveGuard,
    one_shot_scope: Option<&HttpOneShotScopeAuthority>,
) -> HttpDeleteOutcome {
    let Some(session_id) = session_id else {
        return HttpDeleteOutcome::Confirmed;
    };
    let (one_shot_cleanup, remote_delete_is_safe_without_request) = match one_shot_scope {
        Some(authority) => match begin_http_cleanup_quarantine(
            authority,
            cache_key,
            session_id,
            authorized_identity,
        ) {
            Ok(HttpRemoteCleanupStart::Started(cleanup)) => (Some(cleanup), false),
            Ok(HttpRemoteCleanupStart::SafeWithoutDelete) => (None, true),
            Err(_) => return HttpDeleteOutcome::Ambiguous,
        },
        None => (None, false),
    };
    let one_shot_cleanup_identity = one_shot_cleanup
        .as_ref()
        .map(|cleanup| (cleanup.cache_key.clone(), cleanup.cleanup_id));
    let remote_cleanup = one_shot_cleanup_identity
        .as_ref()
        .map(|(cache_key, cleanup_id)| (cache_key.as_str(), *cleanup_id));
    let (cleanup_id, removed_stream_task, removed_local_session, deferred_wait) =
        match begin_http_cleanup(
            cache_key,
            session_id,
            authorized_identity,
            &guard,
            remote_cleanup,
        ) {
            Ok(HttpCleanupStart::SafeWithoutDelete) => {
                if let Some(cleanup) = one_shot_cleanup {
                    cleanup.finish(HttpDeleteOutcome::Confirmed);
                }
                return HttpDeleteOutcome::Confirmed;
            }
            Ok(HttpCleanupStart::AlreadyBlocked) | Err(_) => {
                if let Some(cleanup) = one_shot_cleanup {
                    cleanup.finish(HttpDeleteOutcome::Ambiguous);
                }
                return HttpDeleteOutcome::Ambiguous;
            }
            Ok(HttpCleanupStart::Started {
                cleanup_id,
                removed_stream_task,
                removed_local_session,
                deferred_wait,
            }) => (
                cleanup_id,
                removed_stream_task,
                removed_local_session,
                deferred_wait,
            ),
        };

    if removed_local_session {
        remove_cached_http_descriptors(cache_key);
    }

    if remote_delete_is_safe_without_request {
        if let Some(task) = removed_stream_task {
            task.abort();
        }
        finish_http_cleanup(cache_key, cleanup_id, HttpDeleteOutcome::Confirmed);
        return HttpDeleteOutcome::Confirmed;
    }

    let task_cleanup = HttpCleanupTaskCleanup {
        cache_key: cache_key.to_string(),
        cleanup_id,
        deferred_key: deferred_wait.as_ref().map(|wait| wait.key.clone()),
    };
    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel();
    let task_server_name = server_name.to_string();
    let task_server = server.clone();
    let task_cache_key = cache_key.to_string();
    let task_session_id = session_id.to_string();
    let task_remote_cleanup_key = one_shot_cleanup_identity
        .as_ref()
        .map(|(cache_key, _)| cache_key.clone());
    let task_authorized_identity = authorized_identity.cloned();
    let handle = tokio::spawn(async move {
        let _task_cleanup = task_cleanup;
        let cleanup_guard = guard;
        let one_shot_cleanup = one_shot_cleanup;
        if start_rx.await.is_err() {
            return;
        }
        let force_uncertain = match deferred_wait.as_ref() {
            Some(wait) => match wait_for_deferred_http_cleanup(wait).await {
                Ok(force_uncertain) => force_uncertain,
                Err(_) => return,
            },
            None => false,
        };
        let replacement_owns_same_id = match task_remote_cleanup_key.as_deref() {
            Some(remote_domain_key) => http_remote_replacement_owns_session_id(
                remote_domain_key,
                &task_cache_key,
                task_authorized_identity.as_ref(),
                &task_session_id,
            ),
            None => Ok(false),
        };
        let observed_outcome = match replacement_owns_same_id {
            Ok(true) => HttpDeleteOutcome::Confirmed,
            Ok(false) => {
                terminate_http_session_id(&task_server_name, &task_server, &task_session_id).await
            }
            Err(_) => HttpDeleteOutcome::Ambiguous,
        };
        let outcome = if force_uncertain {
            HttpDeleteOutcome::Ambiguous
        } else {
            observed_outcome
        };
        finish_http_cleanup(&task_cache_key, cleanup_id, outcome);
        if let Some(cleanup) = one_shot_cleanup {
            cleanup.finish(outcome);
        }
        drop(cleanup_guard);
        let _ = result_tx.send(outcome);
    });
    let registered = match http_runtime_state().lock() {
        Ok(mut state)
            if state
                .cleanups
                .get(cache_key)
                .is_some_and(|cleanup| cleanup.cleanup_id == cleanup_id) =>
        {
            state.cleanup_tasks.insert(
                cache_key.to_string(),
                HttpCleanupTaskEntry {
                    cleanup_id,
                    _handle: handle,
                },
            );
            true
        }
        _ => {
            handle.abort();
            false
        }
    };
    if !registered {
        mark_http_cleanup_uncertain(cache_key, cleanup_id, true);
        if let Some((cache_key, cleanup_id)) = one_shot_cleanup_identity.as_ref() {
            mark_http_cleanup_uncertain(cache_key, *cleanup_id, true);
        }
        return HttpDeleteOutcome::Ambiguous;
    }
    if start_tx.send(()).is_err() {
        mark_http_cleanup_uncertain(cache_key, cleanup_id, true);
        if let Some((cache_key, cleanup_id)) = one_shot_cleanup_identity.as_ref() {
            mark_http_cleanup_uncertain(cache_key, *cleanup_id, true);
        }
        return HttpDeleteOutcome::Ambiguous;
    }
    if let Some(task) = removed_stream_task {
        task.abort();
    }

    let mut wait_guard = HttpCleanupWaitGuard {
        cache_key: cache_key.to_string(),
        cleanup_id,
        completed: false,
    };
    let outcome = result_rx.await.unwrap_or(HttpDeleteOutcome::Ambiguous);
    wait_guard.completed = true;
    outcome
}

#[allow(clippy::too_many_arguments)]
async fn terminate_stale_http_response_session(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    session_id: Option<&str>,
    authorized_identity: Option<&HttpSessionIdentity>,
    authority: &HttpRequestAuthority,
    held_exclusive: Option<HttpKeyExclusiveGuard>,
    one_shot_scope: Option<&HttpOneShotScopeAuthority>,
) -> HttpDeleteOutcome {
    if held_exclusive.is_some() && one_shot_scope.is_none() {
        if let Ok(remote_domain) = http_remote_domain_for_cache_key(cache_key, server) {
            ensure_http_cleanup_uncertain(&remote_domain);
        }
        return HttpDeleteOutcome::Ambiguous;
    }
    let (_owned_remote_guard, owned_remote_scope) = if one_shot_scope.is_none() {
        match http_remote_cleanup_guard(server_name, server, cache_key).await {
            Ok((guard, authority)) => (Some(guard), Some(authority)),
            Err(_) => return HttpDeleteOutcome::Ambiguous,
        }
    } else {
        (None, None)
    };
    let remote_scope = one_shot_scope.or(owned_remote_scope.as_ref());
    if let Some(guard) = held_exclusive {
        if !Arc::ptr_eq(&guard.lease.control, &authority.control.control) {
            return HttpDeleteOutcome::Ambiguous;
        }
        return terminate_http_session_with_guard(
            server_name,
            server,
            cache_key,
            session_id,
            authorized_identity,
            guard,
            remote_scope,
        )
        .await;
    }

    let Ok(guard) = http_key_exclusive_guard(cache_key).await else {
        return HttpDeleteOutcome::Ambiguous;
    };
    if !Arc::ptr_eq(&guard.lease.control, &authority.control.control) {
        return HttpDeleteOutcome::Ambiguous;
    }
    terminate_http_session_with_guard(
        server_name,
        server,
        cache_key,
        session_id,
        authorized_identity,
        guard,
        remote_scope,
    )
    .await
}

async fn terminate_stale_http_identity_session(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    identity: &HttpSessionIdentity,
    control: &HttpControlLease,
) {
    let Ok((_remote_guard, remote_scope)) =
        http_remote_cleanup_guard(server_name, server, cache_key).await
    else {
        return;
    };
    let Ok(guard) = http_key_exclusive_guard(cache_key).await else {
        return;
    };
    if !Arc::ptr_eq(&guard.lease.control, &control.control) {
        return;
    }
    let _ = terminate_http_session_with_guard(
        server_name,
        server,
        cache_key,
        Some(&identity.session_id),
        Some(identity),
        guard,
        Some(&remote_scope),
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
#[cfg(test)]
async fn http_post_json(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
    payload: Value,
    session_id: Option<String>,
    timeout_secs: u64,
) -> Result<Value, String> {
    http_post_json_with_authority(
        server_name,
        server,
        cache_key,
        workspace_root,
        client_capabilities,
        payload,
        session_id,
        timeout_secs,
    )
    .await
    .map(|result| result.value)
}

#[allow(clippy::too_many_arguments)]
async fn http_post_json_with_authority(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
    payload: Value,
    session_id: Option<String>,
    timeout_secs: u64,
) -> Result<HttpPostJsonResult, String> {
    if payload.get("method").and_then(Value::as_str) != Some("initialize") {
        return http_post_json_scoped(
            server_name,
            server,
            cache_key,
            workspace_root,
            client_capabilities,
            payload,
            session_id,
            timeout_secs,
            None,
            None,
        )
        .await;
    }

    let (_remote_guard, remote_scope) =
        http_remote_cleanup_guard(server_name, server, cache_key).await?;
    let mut initialize_attempt = HttpOneShotInitializeAttempt::new(&remote_scope);
    let result = http_post_json_scoped(
        server_name,
        server,
        cache_key,
        workspace_root,
        client_capabilities,
        payload,
        session_id,
        timeout_secs,
        Some(&remote_scope),
        Some(&mut initialize_attempt),
    )
    .await;
    match result {
        Ok(response) => {
            initialize_attempt.complete_handoff();
            Ok(response)
        }
        Err(error) => {
            let cleanup_outcome = if cached_http_session_identity_unchecked(cache_key).is_some() {
                terminate_http_session_scoped(server_name, cache_key, server, Some(&remote_scope))
                    .await
            } else {
                HttpDeleteOutcome::Ambiguous
            };
            initialize_attempt.complete_cleanup(cleanup_outcome);
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn http_post_json_scoped(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
    payload: Value,
    session_id: Option<String>,
    timeout_secs: u64,
    one_shot_scope: Option<&HttpOneShotScopeAuthority>,
    mut one_shot_initialize: Option<&mut HttpOneShotInitializeAttempt>,
) -> Result<HttpPostJsonResult, String> {
    if let Some(authority) = one_shot_scope {
        validate_http_one_shot_scope_authority(authority)?;
    }
    workspace_root
        .validate()
        .map_err(|error| format!("MCP workspace capability invalidated: {error}"))?;
    let request_id = payload.get("id").cloned();
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("request")
        .to_string();
    if method == "initialize" && http_cleanup_blocks_initialize(cache_key) {
        return Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string());
    }
    let mut initialize_guard = if method == "initialize" {
        Some(http_key_exclusive_guard(cache_key).await?)
    } else {
        None
    };
    let mut request_authority =
        capture_http_request_authority(cache_key, session_id.as_deref(), method == "initialize")?;
    let request_identity = request_authority.identity.clone();
    let url = streamable_http_endpoint_url(server)?;
    let client = streamable_http_client_with_timeout(timeout_secs)?;
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

    if !http_request_authority_is_current(cache_key, &request_authority) {
        return Err("HTTP MCP request authority was invalidated before send".to_string());
    }
    if let Some(authority) = one_shot_scope {
        validate_http_one_shot_scope_authority(authority)?;
    }
    if let Some(initialize) = one_shot_initialize.as_mut() {
        initialize.mark_may_have_been_sent();
    }
    let response =
        match send_http_request_with_timeout(request, timeout_secs, "HTTP MCP request").await {
            Ok(response) => response,
            Err(error) => {
                if error.contains("timed out after") {
                    fail_closed_http_timeout_before_cancel(
                        cache_key,
                        server,
                        request_identity.as_ref(),
                        one_shot_scope,
                        &mut request_authority,
                    );
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
                }
                return Err(error);
            }
        };
    if response.status() == HttpStatusCode::NOT_FOUND {
        if let Some(identity) = request_identity.as_ref() {
            request_authority.release_in_flight();
            let cleanup_outcome = terminate_stale_http_response_session(
                server_name,
                server,
                cache_key,
                session_id.as_deref(),
                Some(identity),
                &request_authority,
                initialize_guard.take(),
                one_shot_scope,
            )
            .await;
            if let Some(initialize) = one_shot_initialize.as_mut() {
                initialize.record_cleanup(cleanup_outcome);
            }
            remove_http_session_if_generation(cache_key, identity);
        }
        return Err("HTTP MCP session not found".to_string());
    }
    if response.status() == HttpStatusCode::UNAUTHORIZED {
        request_authority.release_in_flight();
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
        request_authority.release_in_flight();
        return Err(format!("HTTP MCP request failed with {status}: {text}"));
    }

    let next_session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if let Err(error) = workspace_root.validate() {
        request_authority.release_in_flight();
        let cleanup_outcome = terminate_stale_http_response_session(
            server_name,
            server,
            cache_key,
            next_session_id.as_deref().or(session_id.as_deref()),
            request_identity.as_ref(),
            &request_authority,
            initialize_guard.take(),
            one_shot_scope,
        )
        .await;
        if let Some(initialize) = one_shot_initialize.as_mut() {
            initialize.record_cleanup(cleanup_outcome);
        }
        if let Some(identity) = request_identity.as_ref() {
            remove_http_session_if_generation(cache_key, identity);
        }
        return Err(format!("MCP workspace capability invalidated: {error}"));
    }
    let installed_identity = match apply_http_response_session(
        cache_key,
        next_session_id.clone(),
        workspace_root,
        &request_authority,
    ) {
        Ok(identity) => identity,
        Err(error) => {
            request_authority.release_in_flight();
            let cleanup_outcome = terminate_stale_http_response_session(
                server_name,
                server,
                cache_key,
                next_session_id.as_deref().or(session_id.as_deref()),
                request_identity.as_ref(),
                &request_authority,
                initialize_guard.take(),
                one_shot_scope,
            )
            .await;
            if let Some(initialize) = one_shot_initialize.as_mut() {
                initialize.record_cleanup(cleanup_outcome);
            }
            return Err(error);
        }
    };
    if let Some(identity) = installed_identity.as_ref()
        && let Some(initialize) = one_shot_initialize.as_mut()
    {
        initialize.bind_installed_session(cache_key, identity);
    }
    let effective_identity = installed_identity.or(request_identity);
    let descriptor_authority = http_descriptor_authority_from_request(
        cache_key,
        &request_authority,
        effective_identity.as_ref(),
    );
    let effective_session_id = effective_identity
        .as_ref()
        .map(|identity| identity.session_id.as_str());
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if request_id.is_none() && method.starts_with("notifications/") {
        request_authority.release_in_flight();
        return Ok(HttpPostJsonResult {
            value: json!({}),
            descriptor_authority,
        });
    }
    if content_type.contains("text/event-stream") {
        let result = parse_sse_json_response_stream(
            response,
            cache_key,
            request_id.clone(),
            server_name,
            server,
            workspace_root,
            client_capabilities,
            effective_session_id,
            effective_identity.as_ref(),
            timeout_secs,
        )
        .await;
        if result
            .as_ref()
            .is_err_and(|error| is_workspace_capability_error(error))
        {
            request_authority.release_in_flight();
            let cleanup_outcome = terminate_stale_http_response_session(
                server_name,
                server,
                cache_key,
                effective_session_id,
                effective_identity.as_ref(),
                &request_authority,
                initialize_guard.take(),
                one_shot_scope,
            )
            .await;
            if let Some(initialize) = one_shot_initialize.as_mut() {
                initialize.record_cleanup(cleanup_outcome);
            }
            if let Some(identity) = effective_identity.as_ref() {
                remove_http_session_if_generation(cache_key, identity);
            }
        }
        if result
            .as_ref()
            .is_err_and(|error| error.contains("timed out after"))
        {
            let timeout_reason = result
                .as_ref()
                .err()
                .cloned()
                .unwrap_or_else(|| "HTTP MCP SSE response timed out".to_string());
            fail_closed_http_timeout_before_cancel(
                cache_key,
                server,
                effective_identity.as_ref(),
                one_shot_scope,
                &mut request_authority,
            );
            if let Some(request_id) = request_id.clone() {
                send_http_cancelled_notification(
                    server_name,
                    server,
                    effective_session_id,
                    request_id,
                    &timeout_reason,
                )
                .await;
            }
        }
        if result.is_ok()
            && !http_response_identity_is_current(
                cache_key,
                &request_authority,
                effective_identity.as_ref(),
            )
        {
            request_authority.release_in_flight();
            let cleanup_outcome = terminate_stale_http_response_session(
                server_name,
                server,
                cache_key,
                effective_session_id,
                effective_identity.as_ref(),
                &request_authority,
                initialize_guard.take(),
                one_shot_scope,
            )
            .await;
            if let Some(initialize) = one_shot_initialize.as_mut() {
                initialize.record_cleanup(cleanup_outcome);
            }
            return Err("HTTP MCP response was invalidated while streaming".to_string());
        }
        if result.is_ok() {
            request_authority.release_in_flight();
        }
        return result.map(|value| HttpPostJsonResult {
            value,
            descriptor_authority,
        });
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
                fail_closed_http_timeout_before_cancel(
                    cache_key,
                    server,
                    effective_identity.as_ref(),
                    one_shot_scope,
                    &mut request_authority,
                );
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
            }
            return Err(error);
        }
    };
    if let Err(error) = workspace_root.validate() {
        request_authority.release_in_flight();
        let cleanup_outcome = terminate_stale_http_response_session(
            server_name,
            server,
            cache_key,
            effective_session_id,
            effective_identity.as_ref(),
            &request_authority,
            initialize_guard.take(),
            one_shot_scope,
        )
        .await;
        if let Some(initialize) = one_shot_initialize.as_mut() {
            initialize.record_cleanup(cleanup_outcome);
        }
        if let Some(identity) = effective_identity.as_ref() {
            remove_http_session_if_generation(cache_key, identity);
        }
        return Err(format!("MCP workspace capability invalidated: {error}"));
    }
    if !http_response_identity_is_current(
        cache_key,
        &request_authority,
        effective_identity.as_ref(),
    ) {
        request_authority.release_in_flight();
        let cleanup_outcome = terminate_stale_http_response_session(
            server_name,
            server,
            cache_key,
            effective_session_id,
            effective_identity.as_ref(),
            &request_authority,
            initialize_guard.take(),
            one_shot_scope,
        )
        .await;
        if let Some(initialize) = one_shot_initialize.as_mut() {
            initialize.record_cleanup(cleanup_outcome);
        }
        return Err("HTTP MCP response was invalidated while being read".to_string());
    }
    request_authority.release_in_flight();
    serde_json::from_str(&text)
        .map(|value| HttpPostJsonResult {
            value,
            descriptor_authority,
        })
        .map_err(|error| format!("invalid HTTP MCP JSON: {error}"))
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

#[allow(clippy::too_many_arguments)]
async fn parse_sse_json_response_stream(
    response: reqwest::Response,
    cache_key: &str,
    expected_id: Option<Value>,
    server_name: &str,
    server: &JsonMcpServerConfig,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
    session_id: Option<&str>,
    session_identity: Option<&HttpSessionIdentity>,
    timeout_secs: u64,
) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs.max(1));
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut last_response = None;

    loop {
        workspace_root
            .validate()
            .map_err(|error| format!("MCP workspace capability invalidated: {error}"))?;
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(format!(
                "HTTP MCP SSE response timed out after {timeout_secs}s"
            ));
        };
        let chunk = match tokio::time::timeout(remaining, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(error))) => {
                return Err(format!("failed to read HTTP MCP SSE response: {error}"));
            }
            Ok(None) => break,
            Err(_) => {
                return Err(format!(
                    "HTTP MCP SSE response timed out after {timeout_secs}s"
                ));
            }
        };

        buffer.push_str(&String::from_utf8_lossy(&chunk));
        for event in parse_sse_events_from_buffer(&mut buffer) {
            record_sse_event_id(&event, cache_key, session_identity);
            let Some(message) = parse_sse_event_message(&event) else {
                continue;
            };
            handle_http_server_message_async(
                &message,
                cache_key,
                server_name,
                server,
                workspace_root,
                client_capabilities,
                session_id,
                timeout_secs,
            )
            .await?;

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
        record_sse_event_id(&buffer, cache_key, session_identity);
        if let Some(message) = parse_sse_event_message(&buffer) {
            handle_http_server_message_async(
                &message,
                cache_key,
                server_name,
                server,
                workspace_root,
                client_capabilities,
                session_id,
                timeout_secs,
            )
            .await?;
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

#[cfg(test)]
async fn initialize_http_session(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
    start_event_stream: bool,
    timeout_secs: u64,
) -> Result<Option<String>, String> {
    let (_remote_guard, remote_scope) =
        http_remote_cleanup_guard(server_name, server, cache_key).await?;
    initialize_http_session_with_scope(
        server_name,
        server,
        cache_key,
        workspace_root,
        client_capabilities,
        start_event_stream,
        timeout_secs,
        &remote_scope,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn initialize_http_session_with_scope(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
    start_event_stream: bool,
    timeout_secs: u64,
    remote_scope: &HttpOneShotScopeAuthority,
) -> Result<Option<String>, String> {
    let mut initialize_attempt = HttpOneShotInitializeAttempt::new(remote_scope);
    let result = initialize_http_session_scoped(
        server_name,
        server,
        cache_key,
        workspace_root,
        client_capabilities,
        start_event_stream,
        timeout_secs,
        Some(remote_scope),
        Some(&mut initialize_attempt),
    )
    .await;
    match result {
        Ok(session_id) => {
            initialize_attempt.complete_handoff();
            Ok(session_id)
        }
        Err(error) => {
            let cleanup_outcome = if cached_http_session_identity_unchecked(cache_key).is_some() {
                terminate_http_session_scoped(server_name, cache_key, server, Some(remote_scope))
                    .await
            } else {
                HttpDeleteOutcome::Ambiguous
            };
            initialize_attempt.complete_cleanup(cleanup_outcome);
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn get_or_initialize_http_session(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
    start_event_stream: bool,
    timeout_secs: u64,
) -> Result<(Option<String>, bool), String> {
    match http_session_lookup(cache_key) {
        Ok(HttpSessionLookup::Active(session_id)) => return Ok((session_id, true)),
        Ok(HttpSessionLookup::Missing | HttpSessionLookup::Expired(_)) => {}
        Err(error) if error == HTTP_MCP_CLEANUP_UNCERTAIN_ERROR => {
            // A sibling caller may own a Pending endpoint cleanup. The
            // endpoint guard below waits for that owner and then rechecks the
            // generation; an Uncertain tombstone still fails closed there.
        }
        Err(_) => {
            // Preserve the existing invalid-capability cleanup path and its
            // stable error instead of treating a corrupt entry as a cache miss.
            return checked_http_session_id(server_name, server, cache_key)
                .await
                .map(|session_id| (session_id, true));
        }
    }

    // Keep the endpoint authority across the second lookup, any idle DELETE,
    // and the replacement initialize. This closes the gap in which another
    // caller could observe the removed local entry and start a competing
    // generation before cleanup/reinitialization converged.
    let (_remote_guard, remote_scope) =
        http_remote_cleanup_guard_after_pending(server_name, server, cache_key).await?;
    // The controlled idle DELETE temporarily removes the cached Session. Keep
    // the per-cache-key control leased so its endpoint mapping cannot be
    // reclaimed between that removal and the replacement initialize.
    let _cache_control = http_control_lease(cache_key)?;
    checked_http_session_id_scoped(server_name, server, cache_key, Some(&remote_scope)).await?;
    match http_session_lookup(cache_key)? {
        HttpSessionLookup::Active(session_id) => return Ok((session_id, true)),
        HttpSessionLookup::Missing => {}
        HttpSessionLookup::Expired(_) => {
            return Err("HTTP MCP idle Session remained expired after cleanup".to_string());
        }
    }
    let session_id = initialize_http_session_with_scope(
        server_name,
        server,
        cache_key,
        workspace_root,
        client_capabilities,
        start_event_stream,
        timeout_secs,
        &remote_scope,
    )
    .await?;
    Ok((session_id, false))
}

#[allow(clippy::too_many_arguments)]
async fn initialize_http_session_scoped(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    workspace_root: &CheckedWorkspacePath,
    client_capabilities: &McpClientCapabilityPolicy,
    start_event_stream: bool,
    timeout_secs: u64,
    one_shot_scope: Option<&HttpOneShotScopeAuthority>,
    one_shot_initialize: Option<&mut HttpOneShotInitializeAttempt>,
) -> Result<Option<String>, String> {
    let client_capabilities = effective_http_client_capabilities(client_capabilities);
    let capabilities = initialize_capabilities(&client_capabilities);
    let init = match http_post_json_scoped(
        server_name,
        server,
        cache_key,
        workspace_root,
        &client_capabilities,
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
        one_shot_scope,
        one_shot_initialize,
    )
    .await
    {
        Ok(init) => init,
        Err(error) if error == "HTTP MCP initialize authority was superseded" => {
            let session_id =
                checked_http_session_id_scoped(server_name, server, cache_key, one_shot_scope)
                    .await?;
            if start_event_stream && let Some(session_id) = session_id.as_deref() {
                start_http_event_stream(
                    server_name,
                    server,
                    cache_key,
                    session_id,
                    workspace_root,
                    &client_capabilities,
                    timeout_secs,
                )
                .await;
            }
            return session_id
                .map(Some)
                .ok_or_else(|| "HTTP MCP initialize was superseded without a session".to_string());
        }
        Err(error) => return Err(error),
    };
    let init = init.value;
    if let Some(error) = init.get("error") {
        return Err(format!(
            "initialize failed: {}",
            serde_json::to_string(error).unwrap_or_else(|_| error.to_string())
        ));
    }
    let session_id =
        checked_http_session_id_scoped(server_name, server, cache_key, one_shot_scope).await?;
    let initialized = http_post_json_scoped(
        server_name,
        server,
        cache_key,
        workspace_root,
        &client_capabilities,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
        session_id.clone(),
        timeout_secs,
        one_shot_scope,
        None,
    )
    .await;
    if one_shot_scope.is_some() {
        initialized.map_err(|error| format!("notifications/initialized failed: {error}"))?;
    }
    if start_event_stream && let Some(session_id) = session_id.as_deref() {
        start_http_event_stream(
            server_name,
            server,
            cache_key,
            session_id,
            workspace_root,
            &client_capabilities,
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
    let client_capabilities = client_capabilities_for_server(server_name, workspace);
    call_http_server_for_scope(
        server_name,
        config,
        workspace,
        workspace,
        &client_capabilities,
        method,
        params,
    )
    .await
    .map(|result| result.value)
}

async fn call_http_server_for_policy(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let client_capabilities = policy.client_capabilities_for_server(server_name);
    call_http_server_for_scope(
        server_name,
        config,
        workspace,
        policy.cache_namespace(workspace),
        &client_capabilities,
        method,
        params,
    )
    .await
    .map(|result| result.value)
}

#[allow(clippy::too_many_arguments)]
async fn call_http_server_for_scope(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    cache_namespace: &Path,
    client_capabilities: &McpClientCapabilityPolicy,
    method: &str,
    params: Value,
) -> Result<HttpCallResult, String> {
    let server = config
        .mcp_servers
        .get(server_name)
        .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
    if !server.enabled {
        return Err(format!("MCP server '{server_name}' is disabled"));
    }
    let client_capabilities = effective_http_client_capabilities(client_capabilities);
    let timeout_secs = server_timeout_secs(server, config);
    let cache_key = cache_key_for_scope(
        server_name,
        server,
        workspace,
        cache_namespace,
        config,
        &client_capabilities,
    )?;
    let workspace_root = resolve_path_checked(".", workspace)
        .map_err(|error| format!("MCP workspace root is invalid: {error}"))?;
    let (mut session_id, session_was_cached) = get_or_initialize_http_session(
        server_name,
        server,
        &cache_key,
        &workspace_root,
        &client_capabilities,
        true,
        timeout_secs,
    )
    .await?;
    if session_was_cached && let Some(session_id) = session_id.as_deref() {
        start_http_event_stream(
            server_name,
            server,
            &cache_key,
            session_id,
            &workspace_root,
            &client_capabilities,
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
    let response = match http_post_json_with_authority(
        server_name,
        server,
        &cache_key,
        &workspace_root,
        &client_capabilities,
        payload.clone(),
        session_id.clone(),
        timeout_secs,
    )
    .await
    {
        Err(error) if error == "HTTP MCP session not found" => {
            session_id = get_or_initialize_http_session(
                server_name,
                server,
                &cache_key,
                &workspace_root,
                &client_capabilities,
                true,
                timeout_secs,
            )
            .await?
            .0;
            http_post_json_with_authority(
                server_name,
                server,
                &cache_key,
                &workspace_root,
                &client_capabilities,
                payload,
                session_id,
                timeout_secs,
            )
            .await?
        }
        other => other?,
    };
    let HttpPostJsonResult {
        value: response,
        descriptor_authority,
    } = response;
    if let Some(error) = response.get("error") {
        return Err(serde_json::to_string(error).unwrap_or_else(|_| error.to_string()));
    }
    let value = response
        .get("result")
        .cloned()
        .ok_or_else(|| format!("server response missing result for method '{method}'"))?;
    Ok(HttpCallResult {
        value,
        descriptor_authority: Some(descriptor_authority),
    })
}

async fn terminate_http_session_id(
    server_name: &str,
    server: &JsonMcpServerConfig,
    session_id: &str,
) -> HttpDeleteOutcome {
    let Ok(url) = streamable_http_endpoint_url(server) else {
        return HttpDeleteOutcome::Ambiguous;
    };
    let Ok(client) = streamable_http_client_with_timeout(2) else {
        return HttpDeleteOutcome::Ambiguous;
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
    match send_http_request_with_timeout(request, 2, "HTTP MCP session terminate").await {
        Ok(response) => classify_http_delete_status(response.status()),
        Err(_) => HttpDeleteOutcome::Ambiguous,
    }
}

fn classify_http_delete_status(status: HttpStatusCode) -> HttpDeleteOutcome {
    match status {
        // RFC 9110 defines these DELETE responses as actions already enacted.
        HttpStatusCode::OK | HttpStatusCode::NO_CONTENT => HttpDeleteOutcome::Confirmed,
        // MCP uses 404 for an already-terminated Session and 405 when client
        // termination is unsupported; 410 likewise proves the target is gone.
        HttpStatusCode::NOT_FOUND | HttpStatusCode::METHOD_NOT_ALLOWED | HttpStatusCode::GONE => {
            HttpDeleteOutcome::NotApplied
        }
        // 202 explicitly permits deferred execution. Other statuses, including
        // otherwise-successful 2xx codes, do not prove DELETE reached a terminal
        // state and therefore cannot release the per-scope quarantine.
        _ => HttpDeleteOutcome::Ambiguous,
    }
}

async fn checked_http_session_id(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
) -> Result<Option<String>, String> {
    checked_http_session_id_scoped(server_name, server, cache_key, None).await
}

async fn cleanup_expired_http_session(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    observed_identity: &HttpSessionIdentity,
    one_shot_scope: Option<&HttpOneShotScopeAuthority>,
) -> Result<Option<String>, String> {
    let (_owned_remote_guard, owned_remote_scope) = if one_shot_scope.is_none() {
        let (guard, authority) = http_remote_cleanup_guard(server_name, server, cache_key).await?;
        (Some(guard), Some(authority))
    } else {
        (None, None)
    };
    let remote_scope = one_shot_scope.or(owned_remote_scope.as_ref());
    let guard = http_key_exclusive_guard(cache_key).await?;
    match http_session_lookup(cache_key)? {
        HttpSessionLookup::Missing => Ok(None),
        HttpSessionLookup::Active(session_id) => Ok(session_id),
        HttpSessionLookup::Expired(current_identity) => {
            if &current_identity != observed_identity {
                return Err("HTTP MCP idle Session changed before cleanup".to_string());
            }
            match terminate_http_session_with_guard(
                server_name,
                server,
                cache_key,
                Some(&current_identity.session_id),
                Some(&current_identity),
                guard,
                remote_scope,
            )
            .await
            {
                HttpDeleteOutcome::Confirmed | HttpDeleteOutcome::NotApplied => Ok(None),
                HttpDeleteOutcome::Ambiguous => Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string()),
            }
        }
    }
}

async fn checked_http_session_id_scoped(
    server_name: &str,
    server: &JsonMcpServerConfig,
    cache_key: &str,
    one_shot_scope: Option<&HttpOneShotScopeAuthority>,
) -> Result<Option<String>, String> {
    match http_session_lookup(cache_key) {
        Ok(HttpSessionLookup::Missing) => Ok(None),
        Ok(HttpSessionLookup::Active(session_id)) => Ok(session_id),
        Ok(HttpSessionLookup::Expired(identity)) => {
            cleanup_expired_http_session(server_name, server, cache_key, &identity, one_shot_scope)
                .await
        }
        Err(error) => {
            terminate_http_session_scoped(server_name, cache_key, server, one_shot_scope).await;
            Err(error)
        }
    }
}

async fn terminate_http_session(
    server_name: &str,
    cache_key: &str,
    server: &JsonMcpServerConfig,
) -> HttpDeleteOutcome {
    terminate_http_session_scoped(server_name, cache_key, server, None).await
}

async fn terminate_http_session_scoped(
    server_name: &str,
    cache_key: &str,
    server: &JsonMcpServerConfig,
    one_shot_scope: Option<&HttpOneShotScopeAuthority>,
) -> HttpDeleteOutcome {
    if is_http_remote_cleanup_domain_key(cache_key) {
        return if http_cleanup_blocks_initialize(cache_key) {
            HttpDeleteOutcome::Ambiguous
        } else {
            HttpDeleteOutcome::Confirmed
        };
    }
    let (_owned_remote_guard, owned_remote_scope) = if one_shot_scope.is_none() {
        match http_remote_cleanup_guard(server_name, server, cache_key).await {
            Ok((guard, authority)) => (Some(guard), Some(authority)),
            Err(_) => return HttpDeleteOutcome::Ambiguous,
        }
    } else {
        (None, None)
    };
    let remote_scope = one_shot_scope.or(owned_remote_scope.as_ref());
    let Ok(guard) = http_key_exclusive_guard(cache_key).await else {
        return HttpDeleteOutcome::Ambiguous;
    };
    let identity = cached_http_session_identity_unchecked(cache_key);
    let Some(identity) = identity else {
        return if http_cleanup_blocks_initialize(cache_key) {
            HttpDeleteOutcome::Ambiguous
        } else {
            HttpDeleteOutcome::Confirmed
        };
    };
    terminate_http_session_with_guard(
        server_name,
        server,
        cache_key,
        Some(&identity.session_id),
        Some(&identity),
        guard,
        remote_scope,
    )
    .await
}

impl TemporaryMcpSession {
    async fn new(server_name: &str, config: &Config, workspace: &Path) -> Result<Self, String> {
        let client_capabilities = client_capabilities_for_server(server_name, workspace);
        Self::new_for_scope(
            server_name,
            config,
            workspace,
            workspace,
            &client_capabilities,
        )
        .await
    }

    async fn new_for_policy(
        server_name: &str,
        config: &Config,
        workspace: &Path,
        policy: &McpSessionPolicy,
    ) -> Result<Self, String> {
        let client_capabilities = policy.client_capabilities_for_server(server_name);
        Self::new_for_scope(
            server_name,
            config,
            workspace,
            policy.cache_namespace(workspace),
            &client_capabilities,
        )
        .await
    }

    async fn new_for_scope(
        server_name: &str,
        config: &Config,
        workspace: &Path,
        cache_namespace: &Path,
        client_capabilities: &McpClientCapabilityPolicy,
    ) -> Result<Self, String> {
        let server = config
            .mcp_servers
            .get(server_name)
            .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
        if !server.enabled {
            return Err(format!("MCP server '{server_name}' is disabled"));
        }
        if is_streamable_http_server(server) {
            let client_capabilities = effective_http_client_capabilities(client_capabilities);
            let timeout_secs = server_timeout_secs(server, config);
            let base_key = cache_key_for_scope(
                server_name,
                server,
                workspace,
                cache_namespace,
                config,
                &client_capabilities,
            )?;
            let cache_key = one_shot_session_cache_key(&base_key);
            let workspace_root = resolve_path_checked(".", workspace)
                .map_err(|error| format!("MCP workspace root is invalid: {error}"))?;
            let (one_shot_guard, one_shot_scope) =
                http_remote_cleanup_guard(server_name, server, &cache_key).await?;
            if cached_http_session_identity_unchecked(&cache_key).is_some()
                && !matches!(
                    terminate_http_session_scoped(
                        server_name,
                        &cache_key,
                        server,
                        Some(&one_shot_scope),
                    )
                    .await,
                    HttpDeleteOutcome::Confirmed | HttpDeleteOutcome::NotApplied
                )
            {
                return Err(HTTP_MCP_CLEANUP_UNCERTAIN_ERROR.to_string());
            }
            let mut initialize_attempt = HttpOneShotInitializeAttempt::new(&one_shot_scope);
            let session_id = match initialize_http_session_scoped(
                server_name,
                server,
                &cache_key,
                &workspace_root,
                &client_capabilities,
                false,
                timeout_secs,
                Some(&one_shot_scope),
                Some(&mut initialize_attempt),
            )
            .await
            {
                Ok(session_id) => session_id,
                Err(error) => {
                    let cleanup_outcome =
                        if cached_http_session_identity_unchecked(&cache_key).is_some() {
                            terminate_http_session_scoped(
                                server_name,
                                &cache_key,
                                server,
                                Some(&one_shot_scope),
                            )
                            .await
                        } else {
                            HttpDeleteOutcome::Ambiguous
                        };
                    initialize_attempt.complete_cleanup(cleanup_outcome);
                    return Err(error);
                }
            };
            let lifecycle = initialize_attempt.into_lifecycle_owner(&cache_key);
            return Ok(Self::Http(Box::new(TemporaryHttpMcpSession {
                server_name: server_name.to_string(),
                server: server.clone(),
                cache_key,
                one_shot_guard: Some(one_shot_guard),
                one_shot_scope: Some(one_shot_scope),
                lifecycle: Some(lifecycle),
                session_id,
                workspace_root,
                client_capabilities,
                timeout_secs,
            })));
        }

        spawn_server_session_for_scope(
            server_name,
            config,
            workspace,
            cache_namespace,
            client_capabilities,
        )
        .await
        .map(|session| Self::Stdio(Box::new(session)))
    }

    async fn request(
        &mut self,
        _workspace: &Path,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        match self {
            Self::Http(session) => {
                let payload = json!({
                    "jsonrpc": "2.0",
                    "id": next_http_request_id(),
                    "method": method,
                    "params": params,
                });
                let response = http_post_json_scoped(
                    &session.server_name,
                    &session.server,
                    &session.cache_key,
                    &session.workspace_root,
                    &session.client_capabilities,
                    payload,
                    session.session_id.clone(),
                    session.timeout_secs,
                    session.one_shot_scope.as_ref(),
                    None,
                )
                .await?
                .value;
                session.session_id = checked_http_session_id_scoped(
                    &session.server_name,
                    &session.server,
                    &session.cache_key,
                    session.one_shot_scope.as_ref(),
                )
                .await?;
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
            Self::Http(session) => {
                let Some(one_shot_scope) = session.one_shot_scope.as_ref() else {
                    return;
                };
                let outcome = if let Ok(guard) = http_key_exclusive_guard(&session.cache_key).await
                {
                    let identity = cached_http_session_identity_unchecked(&session.cache_key);
                    terminate_http_session_with_guard(
                        &session.server_name,
                        &session.server,
                        &session.cache_key,
                        session.session_id.as_deref(),
                        identity.as_ref(),
                        guard,
                        Some(one_shot_scope),
                    )
                    .await
                } else {
                    ensure_http_cleanup_uncertain(&one_shot_scope.cache_key);
                    HttpDeleteOutcome::Ambiguous
                };
                if let Some(mut lifecycle) = session.lifecycle.take() {
                    lifecycle.complete_cleanup(outcome);
                }
                session.session_id = None;
                session.one_shot_guard.take();
                session.one_shot_scope.take();
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

async fn call_server_once_for_policy(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    let mut session =
        TemporaryMcpSession::new_for_policy(server_name, config, workspace, policy).await?;
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
    call_server_for_scope(server_name, config, workspace, None, method, params).await
}

async fn call_server_for_policy(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: &McpSessionPolicy,
    method: &str,
    params: Value,
) -> Result<Value, String> {
    call_server_for_scope(server_name, config, workspace, Some(policy), method, params).await
}

async fn call_server_for_scope_with_descriptor_authority(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
    method: &str,
    params: Value,
) -> Result<HttpCallResult, String> {
    let server = config
        .mcp_servers
        .get(server_name)
        .ok_or_else(|| format!("unknown MCP server '{server_name}'"))?;
    if !is_streamable_http_server(server) {
        return call_server_for_scope(server_name, config, workspace, policy, method, params)
            .await
            .map(|value| HttpCallResult {
                value,
                descriptor_authority: None,
            });
    }
    match policy {
        Some(policy) => {
            let capabilities = policy.client_capabilities_for_server(server_name);
            call_http_server_for_scope(
                server_name,
                config,
                workspace,
                policy.cache_namespace(workspace),
                &capabilities,
                method,
                params,
            )
            .await
        }
        None => {
            let capabilities = client_capabilities_for_server(server_name, workspace);
            call_http_server_for_scope(
                server_name,
                config,
                workspace,
                workspace,
                &capabilities,
                method,
                params,
            )
            .await
        }
    }
}

async fn call_server_for_scope(
    server_name: &str,
    config: &Config,
    workspace: &Path,
    policy: Option<&McpSessionPolicy>,
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
        return match policy {
            Some(policy) => {
                call_http_server_for_policy(server_name, config, workspace, policy, method, params)
                    .await
            }
            None => call_http_server(server_name, config, workspace, method, params).await,
        };
    }

    let (cache_key, mut session) = match policy {
        Some(policy) => {
            get_or_create_server_session_for_policy(server_name, config, workspace, policy).await?
        }
        None => get_or_create_server_session(server_name, config, workspace).await?,
    };

    for attempt in 0..2 {
        let request_result = {
            let mut guard = session.lock().await;
            let req_result = guard.request(method, params.clone()).await;
            match req_result {
                Ok(result) => return Ok(result),
                Err(error) => {
                    let decorated = guard.decorate_error(error);
                    let workspace_invalid = is_workspace_capability_error(&decorated);
                    let should_reset = workspace_invalid || should_reset_mcp_session(&decorated);
                    if should_reset {
                        guard.shutdown().await;
                    }
                    (decorated, should_reset, workspace_invalid)
                }
            }
        };

        let (error, should_reset, workspace_invalid) = request_result;
        if workspace_invalid {
            remove_cached_server_session(&cache_key, &session);
            return Err(error);
        }
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
        session = match policy {
            Some(policy) => {
                get_or_create_server_session_for_policy(server_name, config, workspace, policy)
                    .await?
                    .1
            }
            None => {
                get_or_create_server_session(server_name, config, workspace)
                    .await?
                    .1
            }
        };
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
        .map_err(|error| format_mcp_stdio_transport_error("write", &error))?;
    stdin
        .flush()
        .await
        .map_err(|error| format_mcp_stdio_transport_error("flush", &error))
}

#[allow(clippy::too_many_arguments)]
async fn read_response<R, W>(
    reader: &mut BufReader<R>,
    stdin: &mut W,
    expected_id: u64,
    stdout_lines: &Arc<Mutex<Vec<String>>>,
    server_name: &str,
    workspace_root: &CheckedWorkspaceChildRoot,
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
    workspace_root: &CheckedWorkspaceChildRoot,
    tool_cache_key: &str,
    client_capabilities: &McpClientCapabilityPolicy,
) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
{
    handle_server_message_with_before_write_hook(
        stdin,
        message,
        stdout_lines,
        server_name,
        workspace_root,
        tool_cache_key,
        client_capabilities,
        &mut || {},
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn handle_server_message_with_before_write_hook<W>(
    stdin: &mut W,
    message: &Value,
    stdout_lines: &Arc<Mutex<Vec<String>>>,
    server_name: &str,
    workspace_root: &CheckedWorkspaceChildRoot,
    tool_cache_key: &str,
    client_capabilities: &McpClientCapabilityPolicy,
    before_write: &mut (dyn FnMut() + Send),
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
                    "result": workspace_roots_result(server_name, workspace_root)?
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
            before_write();
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
