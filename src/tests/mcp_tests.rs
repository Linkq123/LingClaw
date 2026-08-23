use super::*;
use crate::{DEFAULT_PORT, Provider, config::JsonMcpAuthConfig};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Form, State},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures::{FutureExt, stream};
use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command as StdCommand,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const MOCK_MCP_SERVER_SOURCE: &str = include_str!("fixtures/mock_mcp_server.rs");

#[cfg(any(unix, windows))]
#[test]
fn mcp_cwd_capability_does_not_follow_an_intermediate_replacement_before_spawn() {
    let root = std::env::temp_dir().join(format!(
        "lingclaw-mcp-cwd-race-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let workspace = root.join("workspace");
    let original = root.join("original-subtree");
    let outside = root.join("outside");
    fs::create_dir_all(workspace.join("subtree")).expect("create workspace subtree");
    fs::create_dir_all(&outside).expect("create outside directory");
    fs::write(workspace.join("subtree/identity.txt"), "inside").expect("seed inside identity");
    fs::write(outside.join("identity.txt"), "outside").expect("seed outside identity");
    let server = JsonMcpServerConfig {
        transport: None,
        command: "unused".to_string(),
        url: None,
        args: Vec::new(),
        env: HashMap::new(),
        headers: HashMap::new(),
        cwd: Some("subtree".to_string()),
        enabled: true,
        auth: None,
        timeout_secs: None,
    };
    let checked = resolve_server_cwd(&server, &workspace).expect("resolve MCP cwd capability");
    fs::rename(workspace.join("subtree"), &original).expect("rename checked MCP cwd");
    let replacement = workspace.join("subtree");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &replacement).expect("create MCP cwd replacement symlink");
    #[cfg(windows)]
    {
        let output = StdCommand::new("cmd.exe")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(&replacement)
            .arg(&outside)
            .output()
            .expect("run junction command");
        assert!(output.status.success());
    }

    let result = checked.process_cwd();

    #[cfg(unix)]
    fs::remove_file(&replacement).expect("remove replacement symlink");
    #[cfg(windows)]
    fs::remove_dir(&replacement).expect("remove replacement junction");
    assert!(
        result.is_err(),
        "MCP cwd must fail closed after its retained subtree moves"
    );
    drop(result);
    drop(checked);
    fs::remove_dir_all(root).expect("clean MCP cwd fixture");
}

fn test_config_with_mcp() -> Config {
    let mut mcp_servers = HashMap::new();
    mcp_servers.insert(
        "github".to_string(),
        JsonMcpServerConfig {
            transport: None,
            command: "npx".to_string(),
            url: None,
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-github".to_string(),
            ],
            env: HashMap::new(),
            headers: HashMap::new(),
            cwd: None,
            enabled: true,
            auth: None,
            timeout_secs: Some(20),
        },
    );
    Config {
        explicit_primary_model_configured: true,
        provider_catalog_declared: false,
        api_key: "env-key".to_string(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::OpenAI,
        openai_stream_include_usage: false,
        structured_memory: false,

        daily_reflection: false,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers,
        port: DEFAULT_PORT,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        tool_timeout: Duration::from_secs(30),
        sub_agent_timeout: Duration::from_secs(300),
        max_llm_retries: 2,
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
        s3: None,
        enable_state_digest: true,
        enable_task_plan: true,
        enable_groups: true,
    }
}

fn unique_temp_workspace(prefix: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{unique}"))
}

fn mock_server_binary() -> &'static PathBuf {
    static BINARY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    BINARY.get_or_init(|| {
        let helper_dir =
            std::env::temp_dir().join(format!("lingclaw-mcp-test-helper-{}", std::process::id()));
        fs::create_dir_all(&helper_dir).expect("helper dir should exist");
        let source_path = helper_dir.join("mock_mcp_server.rs");
        let binary_path = helper_dir.join(if cfg!(windows) {
            "mock_mcp_server.exe"
        } else {
            "mock_mcp_server"
        });

        fs::write(&source_path, MOCK_MCP_SERVER_SOURCE).expect("helper source should write");
        let status = StdCommand::new("rustc")
            .arg("--edition=2021")
            .arg(&source_path)
            .arg("-o")
            .arg(&binary_path)
            .status()
            .expect("rustc should run");
        assert!(status.success(), "mock MCP server should compile");

        binary_path
    })
}

fn test_config_with_mock_server(mode: &str, log_path: &Path) -> Config {
    let mut config = test_config_with_mcp();
    config.mcp_servers.clear();
    config.mcp_servers.insert(
        "mock".to_string(),
        JsonMcpServerConfig {
            transport: None,
            command: mock_server_binary().display().to_string(),
            url: None,
            args: Vec::new(),
            env: HashMap::from([
                ("LINGCLAW_MCP_MODE".to_string(), mode.to_string()),
                (
                    "LINGCLAW_MCP_LOG".to_string(),
                    log_path.display().to_string(),
                ),
            ]),
            headers: HashMap::new(),
            cwd: None,
            enabled: true,
            auth: None,
            timeout_secs: Some(5),
        },
    );
    config
}

fn test_config_with_streamable_http_server(url: String) -> Config {
    let mut config = test_config_with_mcp();
    config.mcp_servers.clear();
    config.mcp_servers.insert(
        "http".to_string(),
        JsonMcpServerConfig {
            transport: Some("streamable-http".to_string()),
            command: String::new(),
            url: Some(url),
            args: Vec::new(),
            env: HashMap::new(),
            headers: HashMap::new(),
            cwd: None,
            enabled: true,
            auth: None,
            timeout_secs: Some(5),
        },
    );
    config
}

async fn streamable_http_test_handler(
    State(log): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    log.lock()
        .await
        .push(json!({"method": method, "sessionId": session_id, "payload": payload}));
    let id = payload.get("id").cloned().unwrap_or(json!(null));

    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => (
            [("mcp-session-id", "test-session")],
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "http-mock", "version": "1.0"}
                }
            })),
        )
            .into_response(),
        Some("tools/list") => {
            let data = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [{
                        "name": "search",
                        "description": "Search records",
                        "inputSchema": {"type": "object", "properties": {}},
                        "annotations": {
                            "readOnlyHint": true,
                            "destructiveHint": false
                        }
                    }]
                }
            });
            (
                [(CONTENT_TYPE, "text/event-stream")],
                format!("event: message\ndata: {data}\n\n"),
            )
                .into_response()
        }
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

async fn streamable_http_get_stream_handler(
    State(log): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
) -> Response {
    log.lock().await.push(json!({"method": "GET"}));
    (
        [(CONTENT_TYPE, "text/event-stream")],
        "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\n",
    )
        .into_response()
}

async fn resources_only_streamable_http_handler(Json(payload): Json<Value>) -> Response {
    let id = payload.get("id").cloned().unwrap_or(json!(null));

    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => (
            [("mcp-session-id", "resources-only-session")],
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {
                        "resources": {"listChanged": true},
                        "prompts": {"listChanged": true}
                    },
                    "serverInfo": {"name": "resources-only", "version": "1.0"}
                }
            })),
        )
            .into_response(),
        Some("tools/list") => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": "Method not found"}
        }))
        .into_response(),
        Some("resources/list") => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "resources": [{
                    "uri": "memo://one",
                    "name": "Memo One",
                    "description": "A resource-only MCP item",
                    "mimeType": "text/plain"
                }]
            }
        }))
        .into_response(),
        Some("prompts/list") => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "prompts": [{
                    "name": "summarize",
                    "description": "Summarize a resource",
                    "arguments": []
                }]
            }
        }))
        .into_response(),
        Some("notifications/initialized") => Response::builder()
            .status(202)
            .body(Body::empty())
            .expect("empty response should build"),
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

async fn repeating_cursor_streamable_http_handler(
    State(log): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    log.lock()
        .await
        .push(json!({"method": method, "sessionId": session_id, "payload": payload}));
    let id = payload.get("id").cloned().unwrap_or(json!(null));

    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => (
            [("mcp-session-id", "repeat-session")],
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "repeat-cursor", "version": "1.0"}
                }
            })),
        )
            .into_response(),
        Some("tools/list") => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [],
                "nextCursor": "same-cursor"
            }
        }))
        .into_response(),
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

async fn filtered_cursor_streamable_http_handler(
    State(log): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
    Json(payload): Json<Value>,
) -> Response {
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    log.lock()
        .await
        .push(json!({"method": method, "payload": payload}));
    let id = payload.get("id").cloned().unwrap_or(json!(null));

    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => (
            [("mcp-session-id", "filtered-cursor-session")],
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "filtered-cursor", "version": "1.0"}
                }
            })),
        )
            .into_response(),
        Some("tools/list") => {
            let params = payload.get("params").cloned().unwrap_or_else(|| json!({}));
            if params.get("kind").and_then(Value::as_str) != Some("docs") {
                return Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32602, "message": "missing kind filter"}
                }))
                .into_response();
            }
            let cursor = params.get("cursor").and_then(Value::as_str);
            let result = if cursor == Some("next") {
                json!({
                    "tools": [{
                        "name": "second",
                        "description": "Second page",
                        "inputSchema": {"type": "object", "properties": {}}
                    }]
                })
            } else {
                json!({
                    "tools": [{
                        "name": "first",
                        "description": "First page",
                        "inputSchema": {"type": "object", "properties": {}}
                    }],
                    "nextCursor": "next"
                })
            };
            Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
        }
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

#[derive(Default)]
struct SessionBoundCursorState {
    init_count: u64,
    first_list_session: Option<String>,
    log: Vec<Value>,
}

async fn session_bound_cursor_streamable_http_handler(
    State(state): State<Arc<tokio::sync::Mutex<SessionBoundCursorState>>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let id = payload.get("id").cloned().unwrap_or(json!(null));

    {
        let mut guard = state.lock().await;
        guard
            .log
            .push(json!({"method": method, "sessionId": session_id, "payload": payload}));
    }

    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let session_id = {
                let mut guard = state.lock().await;
                guard.init_count += 1;
                format!("cursor-session-{}", guard.init_count)
            };
            let mut response = Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "session-bound-cursor", "version": "1.0"}
                }
            }))
            .into_response();
            response.headers_mut().insert(
                "mcp-session-id",
                axum::http::HeaderValue::from_str(&session_id)
                    .expect("session header should be valid"),
            );
            response
        }
        Some("tools/list") => {
            let params = payload.get("params").cloned().unwrap_or_else(|| json!({}));
            let cursor = params.get("cursor").and_then(Value::as_str);
            let result = if cursor == Some("next") {
                let same_session = {
                    let guard = state.lock().await;
                    guard.first_list_session == session_id
                };
                if !same_session {
                    return Json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32000,
                            "message": "cursor belongs to another MCP session"
                        }
                    }))
                    .into_response();
                }
                json!({
                    "tools": [{
                        "name": "second",
                        "description": "Second page",
                        "inputSchema": {"type": "object", "properties": {}}
                    }]
                })
            } else {
                {
                    let mut guard = state.lock().await;
                    guard.first_list_session = session_id.clone();
                }
                json!({
                    "tools": [{
                        "name": "first",
                        "description": "First page",
                        "inputSchema": {"type": "object", "properties": {}}
                    }],
                    "nextCursor": "next"
                })
            };
            Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
        }
        Some("notifications/initialized") => Response::builder()
            .status(202)
            .body(Body::empty())
            .expect("empty response should build"),
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

async fn hanging_streamable_http_handler(Json(payload): Json<Value>) -> Response {
    let id = payload.get("id").cloned().unwrap_or(json!(null));
    tokio::time::sleep(Duration::from_secs(30)).await;
    Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response()
}

async fn open_sse_streamable_http_handler(
    State(log): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    log.lock()
        .await
        .push(json!({"method": method, "sessionId": session_id, "payload": payload}));
    let id = payload.get("id").cloned().unwrap_or(json!(null));

    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => (
            [("mcp-session-id", "test-session")],
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {
                        "tools": {"listChanged": true},
                        "roots": {"listChanged": false}
                    },
                    "serverInfo": {"name": "http-open-sse", "version": "1.0"}
                }
            })),
        )
            .into_response(),
        Some("tools/list") => {
            let request = json!({
                "jsonrpc": "2.0",
                "id": 99,
                "method": "roots/list",
                "params": {}
            });
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "tools": [{
                        "name": "search",
                        "description": "Search records",
                        "inputSchema": {"type": "object", "properties": {}}
                    }]
                }
            });
            let frame =
                format!("event: message\ndata: {request}\n\nevent: message\ndata: {response}\n\n");
            let body_stream =
                stream::once(async move { Ok::<Bytes, Infallible>(Bytes::from(frame)) })
                    .chain(stream::pending());
            Response::builder()
                .header(CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(body_stream))
                .expect("SSE response should build")
        }
        Some("notifications/initialized") => Response::builder()
            .status(202)
            .body(Body::empty())
            .expect("empty response should build"),
        _ if payload.get("id").is_some() => {
            Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response()
        }
        _ => Response::builder()
            .status(202)
            .body(Body::empty())
            .expect("empty response should build"),
    }
}

async fn delayed_roots_streamable_http_handler(
    State(log): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    log.lock().await.push(json!({
        "method": method,
        "sessionId": session_id,
        "payload": payload,
    }));
    let id = payload.get("id").cloned().unwrap_or(json!(null));

    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => (
            [("mcp-session-id", "delayed-roots-session")],
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "delayed-roots", "version": "1.0"}
                }
            })),
        )
            .into_response(),
        Some("tools/list") => {
            let request = json!({
                "jsonrpc": "2.0",
                "id": 9002,
                "method": "roots/list",
                "params": {}
            });
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"tools": []}
            });
            let frame =
                format!("event: message\ndata: {request}\n\nevent: message\ndata: {response}\n\n");
            let body_stream = stream::once(async move {
                tokio::time::sleep(Duration::from_millis(200)).await;
                Ok::<Bytes, Infallible>(Bytes::from(frame))
            });
            Response::builder()
                .header(CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(body_stream))
                .expect("delayed roots SSE response should build")
        }
        Some("notifications/initialized") => Response::builder()
            .status(202)
            .body(Body::empty())
            .expect("empty response should build"),
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

#[derive(Default)]
struct DelayedDeleteState {
    delete_started: tokio::sync::Notify,
    release_delete: tokio::sync::Notify,
    log: tokio::sync::Mutex<Vec<Value>>,
}

#[derive(Default)]
struct DelayedPostEpochState {
    initialize_started: tokio::sync::Notify,
    release_initialize: tokio::sync::Notify,
    normal_started: tokio::sync::Notify,
    release_normal: tokio::sync::Notify,
    deleted_sessions: tokio::sync::Mutex<Vec<String>>,
}

#[derive(Default)]
struct SettingsInvalidateHttpState {
    first_initialize_started: tokio::sync::Notify,
    release_first_initialize: tokio::sync::Notify,
    initialize_count: AtomicUsize,
    deleted_sessions: tokio::sync::Mutex<Vec<String>>,
}

struct SameIdReuseHttpState {
    old_response_started: tokio::sync::Notify,
    release_old_response: tokio::sync::Notify,
    delete_started: tokio::sync::Notify,
    release_delete: tokio::sync::Notify,
    delete_finished: tokio::sync::Notify,
    initialize_started: tokio::sync::Notify,
    next_generation: AtomicUsize,
    active_generation: AtomicUsize,
    delete_count: AtomicUsize,
    normal_request_count: AtomicUsize,
    unexpected_request_count: AtomicUsize,
    deleted_generations: tokio::sync::Mutex<Vec<usize>>,
    delete_status: StatusCode,
    defer_delete_side_effect: bool,
}

#[derive(Default)]
struct DeferredCleanupHttpState {
    initialize_started: tokio::sync::Notify,
    release_initialize: tokio::sync::Notify,
    block_initialize: AtomicBool,
    first_not_found_started: tokio::sync::Notify,
    release_first_not_found: tokio::sync::Notify,
    second_request_started: tokio::sync::Notify,
    release_second_request: tokio::sync::Notify,
    send_timeout_started: tokio::sync::Notify,
    release_send_timeout: tokio::sync::Notify,
    body_timeout_started: tokio::sync::Notify,
    sse_timeout_started: tokio::sync::Notify,
    cancellation_started: tokio::sync::Notify,
    release_cancellation: tokio::sync::Notify,
    block_cancellation: AtomicBool,
    delete_started: tokio::sync::Notify,
    release_delete: tokio::sync::Notify,
    initialize_count: AtomicUsize,
    not_found_count: AtomicUsize,
    delete_count: AtomicUsize,
    delete_status: AtomicUsize,
    cancellation_count: AtomicUsize,
    ordinary_request_count: AtomicUsize,
    ordinary_methods: tokio::sync::Mutex<Vec<String>>,
}

#[derive(Clone, Copy, Debug)]
enum DeferredTimeoutKind {
    Send,
    Body,
    Sse,
}

impl DeferredTimeoutKind {
    fn method(self) -> &'static str {
        match self {
            Self::Send => "deferred/send-timeout",
            Self::Body => "deferred/body-timeout",
            Self::Sse => "deferred/sse-timeout",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Body => "body",
            Self::Sse => "sse",
        }
    }

    fn started(self, state: &DeferredCleanupHttpState) -> &tokio::sync::Notify {
        match self {
            Self::Send => &state.send_timeout_started,
            Self::Body => &state.body_timeout_started,
            Self::Sse => &state.sse_timeout_started,
        }
    }

    fn release_origin(self, state: &DeferredCleanupHttpState) {
        if matches!(self, Self::Send) {
            state.release_send_timeout.notify_waiters();
        }
    }
}

#[derive(Default)]
struct BlockedInitializeHttpState {
    initialize_started: tokio::sync::Notify,
    release_first_initialize: tokio::sync::Notify,
    first_initialize_finished: tokio::sync::Notify,
    initialize_count: AtomicUsize,
    delete_count: AtomicUsize,
}

#[derive(Default)]
struct BlockedInitializedNotificationHttpState {
    notification_started: tokio::sync::Notify,
    release_notification: tokio::sync::Notify,
    initialize_count: AtomicUsize,
    normal_request_count: AtomicUsize,
    delete_count: AtomicUsize,
}

struct InitializeFailureHttpState {
    mode: AtomicUsize,
    delete_status: StatusCode,
    initialize_count: AtomicUsize,
    initialized_notification_count: AtomicUsize,
    delete_count: AtomicUsize,
}

#[derive(Default)]
struct RedirectTransportHttpState {
    alias_request_count: AtomicUsize,
    target_request_count: AtomicUsize,
}

#[derive(Default)]
struct BlockedOneShotRequestHttpState {
    initialize_count: AtomicUsize,
    tool_call_started: tokio::sync::Notify,
    release_tool_call: tokio::sync::Notify,
    delete_count: AtomicUsize,
}

impl InitializeFailureHttpState {
    fn new(mode: usize, delete_status: StatusCode) -> Self {
        Self {
            mode: AtomicUsize::new(mode),
            delete_status,
            initialize_count: AtomicUsize::new(0),
            initialized_notification_count: AtomicUsize::new(0),
            delete_count: AtomicUsize::new(0),
        }
    }
}

impl Default for SameIdReuseHttpState {
    fn default() -> Self {
        Self {
            old_response_started: tokio::sync::Notify::new(),
            release_old_response: tokio::sync::Notify::new(),
            delete_started: tokio::sync::Notify::new(),
            release_delete: tokio::sync::Notify::new(),
            delete_finished: tokio::sync::Notify::new(),
            initialize_started: tokio::sync::Notify::new(),
            next_generation: AtomicUsize::new(2),
            active_generation: AtomicUsize::new(1),
            delete_count: AtomicUsize::new(0),
            normal_request_count: AtomicUsize::new(0),
            unexpected_request_count: AtomicUsize::new(0),
            deleted_generations: tokio::sync::Mutex::new(Vec::new()),
            delete_status: StatusCode::NO_CONTENT,
            defer_delete_side_effect: false,
        }
    }
}

async fn delayed_epoch_http_post(
    State(state): State<Arc<DelayedPostEpochState>>,
    Json(payload): Json<Value>,
) -> Response {
    let id = payload.get("id").cloned().unwrap_or(json!(null));
    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            state.initialize_started.notify_one();
            state.release_initialize.notified().await;
            (
                [("mcp-session-id", "late-initialize-session")],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": {"name": "delayed-epoch", "version": "1.0"}
                    }
                })),
            )
                .into_response()
        }
        Some("tools/list") => {
            state.normal_started.notify_one();
            state.release_normal.notified().await;
            (
                [("mcp-session-id", "old-normal-session")],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {"tools": []}
                })),
            )
                .into_response()
        }
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

async fn delayed_epoch_http_delete(
    State(state): State<Arc<DelayedPostEpochState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(session_id) = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
    {
        state
            .deleted_sessions
            .lock()
            .await
            .push(session_id.to_string());
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn settings_invalidate_http_post(
    State(state): State<Arc<SettingsInvalidateHttpState>>,
    Json(payload): Json<Value>,
) -> Response {
    let id = payload.get("id").cloned().unwrap_or(json!(null));
    if payload.get("method").and_then(Value::as_str) != Some("initialize") {
        return Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response();
    }
    let request_number = state.initialize_count.fetch_add(1, Ordering::AcqRel) + 1;
    if request_number == 1 {
        state.first_initialize_started.notify_one();
        state.release_first_initialize.notified().await;
    }
    let session_id = if request_number == 1 {
        "settings-old-session"
    } else {
        "settings-new-session"
    };
    (
        [("mcp-session-id", session_id)],
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "serverInfo": {"name": "settings-invalidate", "version": "1.0"}
            }
        })),
    )
        .into_response()
}

async fn settings_invalidate_http_delete(
    State(state): State<Arc<SettingsInvalidateHttpState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(session_id) = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
    {
        state
            .deleted_sessions
            .lock()
            .await
            .push(session_id.to_string());
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn same_id_reuse_http_post(
    State(state): State<Arc<SameIdReuseHttpState>>,
    Json(payload): Json<Value>,
) -> Response {
    let id = payload.get("id").cloned().unwrap_or(json!(null));
    match payload.get("method").and_then(Value::as_str) {
        Some("tools/list") => {
            state.old_response_started.notify_one();
            state.release_old_response.notified().await;
            (
                [("mcp-session-id", "fixed-session-id")],
                Json(json!({"jsonrpc": "2.0", "id": id, "result": {"tools": []}})),
            )
                .into_response()
        }
        Some("initialize") => {
            let generation = state.next_generation.fetch_add(1, Ordering::AcqRel);
            state.active_generation.store(generation, Ordering::Release);
            state.initialize_started.notify_one();
            (
                [("mcp-session-id", "fixed-session-id")],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": {"name": "same-id-reuse", "version": "1.0"}
                    }
                })),
            )
                .into_response()
        }
        _ => {
            state.normal_request_count.fetch_add(1, Ordering::AcqRel);
            Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response()
        }
    }
}

async fn same_id_reuse_http_delete(State(state): State<Arc<SameIdReuseHttpState>>) -> Response {
    state.delete_count.fetch_add(1, Ordering::AcqRel);
    state.delete_started.notify_one();
    let status = state.delete_status;
    let (response_tx, response_rx) = tokio::sync::oneshot::channel();
    let side_effect_state = state.clone();
    tokio::spawn(async move {
        side_effect_state.release_delete.notified().await;
        let generation = side_effect_state
            .active_generation
            .swap(0, Ordering::AcqRel);
        side_effect_state
            .deleted_generations
            .lock()
            .await
            .push(generation);
        side_effect_state.delete_finished.notify_waiters();
        let _ = response_tx.send(());
    });
    if state.defer_delete_side_effect {
        return status.into_response();
    }
    let _ = response_rx.await;
    status.into_response()
}

async fn deferred_cleanup_http_post(
    State(state): State<Arc<DeferredCleanupHttpState>>,
    Json(payload): Json<Value>,
) -> Response {
    let id = payload.get("id").cloned().unwrap_or(json!(null));
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if method != "initialize" && method != "notifications/initialized" {
        state.ordinary_methods.lock().await.push(method.to_string());
    }
    match Some(method) {
        Some("initialize") => {
            state.initialize_count.fetch_add(1, Ordering::AcqRel);
            if state.block_initialize.load(Ordering::Acquire) {
                state.initialize_started.notify_one();
                state.release_initialize.notified().await;
            }
            (
                [("mcp-session-id", "deferred-fixed-session")],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": {"name": "deferred-cleanup", "version": "1.0"}
                    }
                })),
            )
                .into_response()
        }
        Some("notifications/initialized") => Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Body::empty())
            .expect("empty initialized response should build"),
        Some("notifications/cancelled") => {
            state.cancellation_count.fetch_add(1, Ordering::AcqRel);
            state.cancellation_started.notify_one();
            if state.block_cancellation.load(Ordering::Acquire) {
                state.release_cancellation.notified().await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Some("deferred/first-not-found") => {
            state.ordinary_request_count.fetch_add(1, Ordering::AcqRel);
            if state.not_found_count.fetch_add(1, Ordering::AcqRel) == 0 {
                state.first_not_found_started.notify_one();
                state.release_first_not_found.notified().await;
                StatusCode::NOT_FOUND.into_response()
            } else {
                Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response()
            }
        }
        Some("deferred/second") => {
            state.ordinary_request_count.fetch_add(1, Ordering::AcqRel);
            state.second_request_started.notify_one();
            state.release_second_request.notified().await;
            (
                [("mcp-session-id", "deferred-fixed-session")],
                Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
            )
                .into_response()
        }
        Some("deferred/send-timeout") => {
            state.ordinary_request_count.fetch_add(1, Ordering::AcqRel);
            state.send_timeout_started.notify_one();
            state.release_send_timeout.notified().await;
            Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response()
        }
        Some("deferred/body-timeout") => {
            state.ordinary_request_count.fetch_add(1, Ordering::AcqRel);
            state.body_timeout_started.notify_one();
            let body_stream = stream::pending::<Result<Bytes, Infallible>>();
            Response::builder()
                .header(CONTENT_TYPE, "application/json")
                .header("mcp-session-id", "deferred-fixed-session")
                .body(Body::from_stream(body_stream))
                .expect("pending response body should build")
        }
        Some("deferred/sse-timeout") => {
            state.ordinary_request_count.fetch_add(1, Ordering::AcqRel);
            state.sse_timeout_started.notify_one();
            let body_stream = stream::pending::<Result<Bytes, Infallible>>();
            Response::builder()
                .header(CONTENT_TYPE, "text/event-stream")
                .header("mcp-session-id", "deferred-fixed-session")
                .body(Body::from_stream(body_stream))
                .expect("pending SSE response should build")
        }
        _ => {
            state.ordinary_request_count.fetch_add(1, Ordering::AcqRel);
            Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response()
        }
    }
}

async fn deferred_cleanup_http_delete(
    State(state): State<Arc<DeferredCleanupHttpState>>,
) -> Response {
    state.delete_count.fetch_add(1, Ordering::AcqRel);
    state.delete_started.notify_one();
    state.release_delete.notified().await;
    let configured = state.delete_status.load(Ordering::Acquire);
    let status = u16::try_from(configured)
        .ok()
        .and_then(|status| StatusCode::from_u16(status).ok())
        .unwrap_or(StatusCode::NO_CONTENT);
    status.into_response()
}

async fn same_id_unexpected_http_request(
    State(state): State<Arc<SameIdReuseHttpState>>,
) -> Response {
    state
        .unexpected_request_count
        .fetch_add(1, Ordering::AcqRel);
    StatusCode::NOT_FOUND.into_response()
}

async fn blocked_initialize_http_post(
    State(state): State<Arc<BlockedInitializeHttpState>>,
    Json(payload): Json<Value>,
) -> Response {
    let id = payload.get("id").cloned().unwrap_or(json!(null));
    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let request_number = state.initialize_count.fetch_add(1, Ordering::AcqRel) + 1;
            if request_number == 1 {
                state.initialize_started.notify_one();
                let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
                let side_effect_state = state.clone();
                tokio::spawn(async move {
                    side_effect_state.release_first_initialize.notified().await;
                    side_effect_state.first_initialize_finished.notify_waiters();
                    let _ = finished_tx.send(());
                });
                let _ = finished_rx.await;
            }
            (
                [("mcp-session-id", "fixed-initialize-session")],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": {"name": "blocked-initialize", "version": "1.0"}
                    }
                })),
            )
                .into_response()
        }
        Some("notifications/initialized") => Response::builder()
            .status(StatusCode::ACCEPTED)
            .body(Body::empty())
            .expect("empty initialized response should build"),
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

async fn blocked_initialize_http_delete(
    State(state): State<Arc<BlockedInitializeHttpState>>,
) -> Response {
    state.delete_count.fetch_add(1, Ordering::AcqRel);
    StatusCode::NO_CONTENT.into_response()
}

async fn blocked_initialized_notification_http_post(
    State(state): State<Arc<BlockedInitializedNotificationHttpState>>,
    Json(payload): Json<Value>,
) -> Response {
    let id = payload.get("id").cloned().unwrap_or(json!(null));
    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            state.initialize_count.fetch_add(1, Ordering::AcqRel);
            (
                [("mcp-session-id", "installed-before-notification")],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": {"name": "blocked-notification", "version": "1.0"}
                    }
                })),
            )
                .into_response()
        }
        Some("notifications/initialized") => {
            state.notification_started.notify_one();
            state.release_notification.notified().await;
            StatusCode::ACCEPTED.into_response()
        }
        _ => {
            state.normal_request_count.fetch_add(1, Ordering::AcqRel);
            Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response()
        }
    }
}

async fn blocked_initialized_notification_http_delete(
    State(state): State<Arc<BlockedInitializedNotificationHttpState>>,
) -> Response {
    state.delete_count.fetch_add(1, Ordering::AcqRel);
    StatusCode::NO_CONTENT.into_response()
}

async fn initialize_failure_http_post(
    State(state): State<Arc<InitializeFailureHttpState>>,
    Json(payload): Json<Value>,
) -> Response {
    let id = payload.get("id").cloned().unwrap_or(json!(null));
    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            state.initialize_count.fetch_add(1, Ordering::AcqRel);
            match state.mode.load(Ordering::Acquire) {
                0 => (
                    [("mcp-session-id", "known-failure-session")],
                    Json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32603, "message": "initialize rejected"}
                    })),
                )
                    .into_response(),
                3 => Response::builder()
                    .status(StatusCode::OK)
                    .header("mcp-session-id", "known-failure-session")
                    .header(CONTENT_TYPE, "application/json")
                    .body(Body::from("{"))
                    .expect("malformed initialize response should build"),
                _ => (
                    [("mcp-session-id", "known-failure-session")],
                    Json(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": "2025-11-25",
                            "capabilities": {},
                            "serverInfo": {"name": "initialize-failure", "version": "1.0"}
                        }
                    })),
                )
                    .into_response(),
            }
        }
        Some("notifications/initialized") => {
            state
                .initialized_notification_count
                .fetch_add(1, Ordering::AcqRel);
            if state.mode.load(Ordering::Acquire) == 2 {
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            } else {
                StatusCode::ACCEPTED.into_response()
            }
        }
        Some("tools/call") if state.mode.load(Ordering::Acquire) == 4 => {
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

async fn initialize_failure_http_delete(
    State(state): State<Arc<InitializeFailureHttpState>>,
) -> Response {
    state.delete_count.fetch_add(1, Ordering::AcqRel);
    state.delete_status.into_response()
}

async fn redirect_transport_307(State(state): State<Arc<RedirectTransportHttpState>>) -> Response {
    state.alias_request_count.fetch_add(1, Ordering::AcqRel);
    Response::builder()
        .status(StatusCode::TEMPORARY_REDIRECT)
        .header("location", "/target")
        .body(Body::empty())
        .expect("307 redirect response should build")
}

async fn redirect_transport_308(State(state): State<Arc<RedirectTransportHttpState>>) -> Response {
    state.alias_request_count.fetch_add(1, Ordering::AcqRel);
    Response::builder()
        .status(StatusCode::PERMANENT_REDIRECT)
        .header("location", "/target")
        .body(Body::empty())
        .expect("308 redirect response should build")
}

async fn redirect_transport_target(
    State(state): State<Arc<RedirectTransportHttpState>>,
) -> Response {
    state.target_request_count.fetch_add(1, Ordering::AcqRel);
    Json(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "serverInfo": {"name": "redirect-target", "version": "1.0"}
        }
    }))
    .into_response()
}

async fn blocked_one_shot_request_http_post(
    State(state): State<Arc<BlockedOneShotRequestHttpState>>,
    Json(payload): Json<Value>,
) -> Response {
    let id = payload.get("id").cloned().unwrap_or(json!(null));
    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            state.initialize_count.fetch_add(1, Ordering::AcqRel);
            (
                [("mcp-session-id", "blocked-one-shot-session")],
                Json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "serverInfo": {"name": "blocked-request", "version": "1.0"}
                    }
                })),
            )
                .into_response()
        }
        Some("tools/call") => {
            state.tool_call_started.notify_one();
            state.release_tool_call.notified().await;
            Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response()
        }
        Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

async fn blocked_one_shot_request_http_delete(
    State(state): State<Arc<BlockedOneShotRequestHttpState>>,
) -> Response {
    state.delete_count.fetch_add(1, Ordering::AcqRel);
    StatusCode::NO_CONTENT.into_response()
}

async fn delayed_http_delete(
    State(state): State<Arc<DelayedDeleteState>>,
    headers: HeaderMap,
) -> Response {
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    state
        .log
        .lock()
        .await
        .push(json!({"method": "DELETE", "sessionId": session_id}));
    state.delete_started.notify_one();
    state.release_delete.notified().await;
    StatusCode::NO_CONTENT.into_response()
}

async fn auth_recording_streamable_http_handler(
    State(log): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    log.lock()
        .await
        .push(json!({"method": method, "authorization": authorization}));
    let id = payload.get("id").cloned().unwrap_or(json!(null));

    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => (
            [("mcp-session-id", "auth-session")],
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "auth-mock", "version": "1.0"}
                }
            })),
        )
            .into_response(),
        Some("tools/list") => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [{
                    "name": "write_record",
                    "description": "Update a record",
                    "inputSchema": {"type": "object", "properties": {}}
                }]
            }
        }))
        .into_response(),
        Some("tools/call") => {
            tokio::time::sleep(Duration::from_secs(30)).await;
            Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response()
        }
        Some("notifications/initialized") | Some("notifications/cancelled") => Response::builder()
            .status(202)
            .body(Body::empty())
            .expect("empty response should build"),
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

async fn auth_recording_streamable_http_delete(
    State(log): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
    headers: HeaderMap,
) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    log.lock()
        .await
        .push(json!({"method": "DELETE", "authorization": authorization}));
    Response::builder()
        .status(204)
        .body(Body::empty())
        .expect("empty response should build")
}

async fn timeout_sse_streamable_http_handler(
    State(log): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let method = payload
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    log.lock()
        .await
        .push(json!({"method": method, "sessionId": session_id}));
    let id = payload.get("id").cloned().unwrap_or(json!(null));

    match payload.get("method").and_then(Value::as_str) {
        Some("initialize") => (
            [("mcp-session-id", "timeout-sse-session")],
            Json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "timeout-sse", "version": "1.0"}
                }
            })),
        )
            .into_response(),
        Some("tools/list") => Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "tools": [{
                    "name": "write_record",
                    "description": "Update a record",
                    "inputSchema": {"type": "object", "properties": {}}
                }]
            }
        }))
        .into_response(),
        Some("tools/call") => {
            let body_stream = stream::pending::<Result<Bytes, Infallible>>();
            Response::builder()
                .header(CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(body_stream))
                .expect("SSE response should build")
        }
        Some("notifications/initialized") | Some("notifications/cancelled") => Response::builder()
            .status(202)
            .body(Body::empty())
            .expect("empty response should build"),
        _ => Json(json!({"jsonrpc": "2.0", "id": id, "result": {}})).into_response(),
    }
}

async fn oauth_refresh_token_handler(
    State(log): State<Arc<tokio::sync::Mutex<Vec<Value>>>>,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    log.lock().await.push(json!(form));
    Json(json!({
        "access_token": "new-access-token",
        "refresh_token": "new-refresh-token",
        "expires_in": 3600,
        "scope": "read write"
    }))
}

async fn oauth_protected_resource_metadata(State(base): State<String>) -> impl IntoResponse {
    Json(json!({
        "resource": format!("{base}mcp"),
        "authorization_servers": [format!("{base}auth")]
    }))
}

async fn oauth_protected_resource_metadata_with_path_issuer(
    State(base): State<String>,
) -> impl IntoResponse {
    Json(json!({
        "resource": format!("{base}mcp"),
        "authorization_servers": [format!("{base}tenant")]
    }))
}

async fn oauth_authorization_server_metadata(State(base): State<String>) -> impl IntoResponse {
    Json(json!({
        "authorization_endpoint": format!("{base}authorize"),
        "token_endpoint": format!("{base}token")
    }))
}

async fn oauth_path_authorization_server_metadata(State(base): State<String>) -> impl IntoResponse {
    Json(json!({
        "authorization_endpoint": format!("{base}tenant/authorize"),
        "token_endpoint": format!("{base}tenant/token")
    }))
}

async fn spawn_streamable_http_test_server() -> (
    String,
    Arc<tokio::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/",
            get(streamable_http_get_stream_handler).post(streamable_http_test_handler),
        )
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP test listener should bind");
    let addr = listener.local_addr().expect("HTTP test listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), log, handle)
}

async fn spawn_resources_only_streamable_http_test_server() -> (String, tokio::task::JoinHandle<()>)
{
    let app = Router::new().route("/", post(resources_only_streamable_http_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP resources-only test listener should bind");
    let addr = listener
        .local_addr()
        .expect("HTTP resources-only test listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), handle)
}

async fn spawn_oauth_metadata_test_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("OAuth metadata test listener should bind");
    let addr = listener
        .local_addr()
        .expect("OAuth metadata test listener address");
    let base = format!("http://{addr}/");
    let app = Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth_protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth_protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_authorization_server_metadata),
        )
        .with_state(base.clone());
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, handle)
}

async fn spawn_oauth_path_issuer_metadata_test_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("OAuth metadata test listener should bind");
    let addr = listener
        .local_addr()
        .expect("OAuth metadata test listener address");
    let base = format!("http://{addr}/");
    let app = Router::new()
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth_protected_resource_metadata_with_path_issuer),
        )
        .route(
            "/.well-known/oauth-authorization-server/tenant",
            get(oauth_path_authorization_server_metadata),
        )
        .with_state(base.clone());
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, handle)
}

async fn spawn_oauth_path_issuer_oidc_metadata_test_server() -> (String, tokio::task::JoinHandle<()>)
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("OAuth metadata test listener should bind");
    let addr = listener
        .local_addr()
        .expect("OAuth metadata test listener address");
    let base = format!("http://{addr}/");
    let app = Router::new()
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth_protected_resource_metadata_with_path_issuer),
        )
        .route(
            "/tenant/.well-known/openid-configuration",
            get(oauth_path_authorization_server_metadata),
        )
        .with_state(base.clone());
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (base, handle)
}

async fn spawn_repeating_cursor_streamable_http_test_server() -> (
    String,
    Arc<tokio::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/", post(repeating_cursor_streamable_http_handler))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP repeating cursor test listener should bind");
    let addr = listener
        .local_addr()
        .expect("HTTP repeating cursor test listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), log, handle)
}

async fn spawn_filtered_cursor_streamable_http_test_server() -> (
    String,
    Arc<tokio::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/", post(filtered_cursor_streamable_http_handler))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP filtered cursor test listener should bind");
    let addr = listener
        .local_addr()
        .expect("HTTP filtered cursor test listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), log, handle)
}

async fn spawn_session_bound_cursor_streamable_http_test_server() -> (
    String,
    Arc<tokio::sync::Mutex<SessionBoundCursorState>>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(tokio::sync::Mutex::new(SessionBoundCursorState::default()));
    let app = Router::new()
        .route("/", post(session_bound_cursor_streamable_http_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP session-bound cursor test listener should bind");
    let addr = listener
        .local_addr()
        .expect("HTTP session-bound cursor test listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), state, handle)
}

async fn spawn_oauth_token_test_server() -> (
    String,
    Arc<tokio::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/token", post(oauth_refresh_token_handler))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("OAuth test listener should bind");
    let addr = listener.local_addr().expect("OAuth test listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), log, handle)
}

async fn spawn_hanging_streamable_http_test_server() -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route("/", post(hanging_streamable_http_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP test listener should bind");
    let addr = listener.local_addr().expect("HTTP test listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), handle)
}

async fn spawn_open_sse_streamable_http_test_server() -> (
    String,
    Arc<tokio::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/", post(open_sse_streamable_http_handler))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP test listener should bind");
    let addr = listener.local_addr().expect("HTTP test listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), log, handle)
}

async fn spawn_delayed_roots_streamable_http_test_server() -> (
    String,
    Arc<tokio::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/",
            post(delayed_roots_streamable_http_handler)
                .delete(auth_recording_streamable_http_delete),
        )
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("delayed roots HTTP listener should bind");
    let addr = listener
        .local_addr()
        .expect("delayed roots HTTP listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), log, handle)
}

async fn spawn_delayed_delete_streamable_http_test_server()
-> (String, Arc<DelayedDeleteState>, tokio::task::JoinHandle<()>) {
    let state = Arc::new(DelayedDeleteState::default());
    let app = Router::new()
        .route("/", axum::routing::delete(delayed_http_delete))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("delayed DELETE HTTP listener should bind");
    let addr = listener
        .local_addr()
        .expect("delayed DELETE HTTP listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), state, handle)
}

async fn spawn_delayed_epoch_streamable_http_test_server() -> (
    String,
    Arc<DelayedPostEpochState>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(DelayedPostEpochState::default());
    let app = Router::new()
        .route(
            "/",
            post(delayed_epoch_http_post).delete(delayed_epoch_http_delete),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("delayed epoch listener should bind");
    let addr = listener
        .local_addr()
        .expect("delayed epoch listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), state, handle)
}

async fn spawn_settings_invalidate_http_test_server() -> (
    String,
    Arc<SettingsInvalidateHttpState>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(SettingsInvalidateHttpState::default());
    let app = Router::new()
        .route(
            "/",
            post(settings_invalidate_http_post).delete(settings_invalidate_http_delete),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("settings invalidation listener should bind");
    let addr = listener
        .local_addr()
        .expect("settings invalidation listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), state, handle)
}

async fn spawn_same_id_reuse_http_test_server() -> (
    String,
    Arc<SameIdReuseHttpState>,
    tokio::task::JoinHandle<()>,
) {
    spawn_same_id_reuse_http_test_server_with_delete(StatusCode::NO_CONTENT, false).await
}

async fn spawn_same_id_reuse_http_test_server_with_delete(
    delete_status: StatusCode,
    defer_delete_side_effect: bool,
) -> (
    String,
    Arc<SameIdReuseHttpState>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(SameIdReuseHttpState {
        delete_status,
        defer_delete_side_effect,
        ..Default::default()
    });
    let app = Router::new()
        .route(
            "/",
            post(same_id_reuse_http_post).delete(same_id_reuse_http_delete),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("same-ID reuse listener should bind");
    let addr = listener
        .local_addr()
        .expect("same-ID reuse listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), state, handle)
}

async fn spawn_deferred_cleanup_http_test_server() -> (
    String,
    Arc<DeferredCleanupHttpState>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(DeferredCleanupHttpState::default());
    let app = Router::new()
        .route(
            "/",
            post(deferred_cleanup_http_post).delete(deferred_cleanup_http_delete),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("deferred-cleanup listener should bind");
    let addr = listener
        .local_addr()
        .expect("deferred-cleanup listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), state, handle)
}

async fn spawn_same_id_reuse_http_test_server_at_mcp(
    delete_status: StatusCode,
    defer_delete_side_effect: bool,
) -> (
    String,
    Arc<SameIdReuseHttpState>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(SameIdReuseHttpState {
        delete_status,
        defer_delete_side_effect,
        ..Default::default()
    });
    let app = Router::new()
        .route(
            "/mcp",
            post(same_id_reuse_http_post).delete(same_id_reuse_http_delete),
        )
        .fallback(axum::routing::any(same_id_unexpected_http_request))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("same-ID percent alias listener should bind");
    let addr = listener
        .local_addr()
        .expect("same-ID percent alias listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), state, handle)
}

async fn spawn_blocked_initialize_http_test_server() -> (
    String,
    Arc<BlockedInitializeHttpState>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(BlockedInitializeHttpState::default());
    let app = Router::new()
        .route(
            "/",
            post(blocked_initialize_http_post).delete(blocked_initialize_http_delete),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("blocked initialize listener should bind");
    let addr = listener
        .local_addr()
        .expect("blocked initialize listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), state, handle)
}

async fn spawn_blocked_initialized_notification_http_test_server() -> (
    String,
    Arc<BlockedInitializedNotificationHttpState>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(BlockedInitializedNotificationHttpState::default());
    let app = Router::new()
        .route(
            "/",
            post(blocked_initialized_notification_http_post)
                .delete(blocked_initialized_notification_http_delete),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("blocked initialized-notification listener should bind");
    let addr = listener
        .local_addr()
        .expect("blocked initialized-notification listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), state, handle)
}

async fn spawn_initialize_failure_http_test_server(
    mode: usize,
    delete_status: StatusCode,
) -> (
    String,
    Arc<InitializeFailureHttpState>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(InitializeFailureHttpState::new(mode, delete_status));
    let app = Router::new()
        .route(
            "/",
            post(initialize_failure_http_post).delete(initialize_failure_http_delete),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("initialize failure listener should bind");
    let addr = listener
        .local_addr()
        .expect("initialize failure listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), state, handle)
}

async fn spawn_redirect_transport_http_test_server() -> (
    String,
    Arc<RedirectTransportHttpState>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(RedirectTransportHttpState::default());
    let app = Router::new()
        .route("/alias307", post(redirect_transport_307))
        .route("/alias308", post(redirect_transport_308))
        .route("/target", post(redirect_transport_target))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("redirect transport listener should bind");
    let addr = listener
        .local_addr()
        .expect("redirect transport listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), state, handle)
}

async fn spawn_blocked_one_shot_request_http_test_server() -> (
    String,
    Arc<BlockedOneShotRequestHttpState>,
    tokio::task::JoinHandle<()>,
) {
    let state = Arc::new(BlockedOneShotRequestHttpState::default());
    let app = Router::new()
        .route(
            "/",
            post(blocked_one_shot_request_http_post).delete(blocked_one_shot_request_http_delete),
        )
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("blocked one-shot request listener should bind");
    let addr = listener
        .local_addr()
        .expect("blocked one-shot request listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), state, handle)
}

async fn spawn_auth_recording_streamable_http_test_server() -> (
    String,
    Arc<tokio::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/",
            post(auth_recording_streamable_http_handler)
                .delete(auth_recording_streamable_http_delete),
        )
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP auth test listener should bind");
    let addr = listener
        .local_addr()
        .expect("HTTP auth test listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), log, handle)
}

async fn spawn_timeout_sse_streamable_http_test_server() -> (
    String,
    Arc<tokio::sync::Mutex<Vec<Value>>>,
    tokio::task::JoinHandle<()>,
) {
    let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/", post(timeout_sse_streamable_http_handler))
        .with_state(log.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("HTTP SSE timeout test listener should bind");
    let addr = listener
        .local_addr()
        .expect("HTTP SSE timeout test listener address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/"), log, handle)
}

async fn clear_mcp_caches_for_test() {
    // Tests must never fall back to the developer's real ~/.lingclaw MCP auth
    // file. The suite intentionally reuses common server names such as `http`,
    // so a real token for that name can otherwise make local HTTP fixtures fail
    // resource binding before they receive a request.
    let isolated_auth_path = std::env::temp_dir().join(format!(
        "lingclaw-mcp-test-auth-{}.json",
        std::process::id()
    ));
    let _ = fs::remove_file(&isolated_auth_path);
    set_auth_file_path_for_test(isolated_auth_path);
    if let Some(signals) = MCP_HTTP_EXCLUSIVE_WAIT_SIGNALS.get()
        && let Ok(mut signals) = signals.lock()
    {
        signals.clear();
    }
    if let Some(barrier) = MCP_HTTP_DESCRIPTOR_INSERT_BARRIER.get()
        && let Ok(mut barrier) = barrier.lock()
    {
        barrier.take();
    }
    let sessions = {
        session_cache()
            .lock()
            .map(|mut cache| {
                cache
                    .drain()
                    .map(|(_, entry)| entry.session)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    for session in sessions {
        let mut guard = session.lock().await;
        guard.shutdown().await;
    }

    let http_tasks = match http_runtime_state().lock() {
        Ok(mut state) => {
            let mut tasks = state
                .stream_tasks
                .drain()
                .map(|(_, entry)| entry.handle)
                .collect::<Vec<_>>();
            tasks.extend(state.cleanup_tasks.drain().map(|(_, entry)| entry._handle));
            tasks
        }
        Err(_) => Vec::new(),
    };
    for task in &http_tasks {
        task.abort();
    }
    for task in http_tasks {
        let _ = task.await;
    }

    if let Ok(mut state) = http_runtime_state().lock() {
        state.sessions.clear();
        state.last_event_ids.clear();
        state.cleanups.clear();
        state.controls.clear();
        state.remote_domains_by_server.clear();
        state.remote_domain_by_cache_key.clear();
        state.supersessions.clear();
        state.in_flight_requests.clear();
        state.deferred_cleanups.clear();
    }
    if let Ok(mut cache) = tool_cache().lock() {
        cache.clear();
    }
    if let Ok(mut cache) = resource_cache().lock() {
        cache.clear();
    }
    if let Ok(mut cache) = prompt_cache().lock() {
        cache.clear();
    }
}

struct PanicSafeMcpWorkerState {
    completed: AtomicBool,
    completed_notify: tokio::sync::Notify,
}

impl PanicSafeMcpWorkerState {
    fn new() -> Self {
        Self {
            completed: AtomicBool::new(false),
            completed_notify: tokio::sync::Notify::new(),
        }
    }

    async fn wait(&self) {
        while !self.completed.load(Ordering::Acquire) {
            let notified = self.completed_notify.notified();
            if self.completed.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
    }
}

struct PanicSafeMcpWorkerCompletion {
    state: Arc<PanicSafeMcpWorkerState>,
}

impl Drop for PanicSafeMcpWorkerCompletion {
    fn drop(&mut self) {
        self.state.completed.store(true, Ordering::Release);
        self.state.completed_notify.notify_waiters();
    }
}

struct PanicSafeMcpWorker {
    abort: tokio::task::AbortHandle,
    state: Arc<PanicSafeMcpWorkerState>,
}

#[derive(Default)]
struct PanicSafeMcpFixture {
    paths: Vec<PathBuf>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    workers: Vec<PanicSafeMcpWorker>,
    barrier_releases: Vec<Box<dyn FnOnce() + Send>>,
    forced_state_cleanup_error: Option<String>,
    forced_path_cleanup_errors: HashMap<PathBuf, String>,
    cleanup_diagnostics: Arc<std::sync::Mutex<Vec<String>>>,
    cleaned: bool,
}

impl PanicSafeMcpFixture {
    fn track_path(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    fn track_task(&mut self, task: tokio::task::JoinHandle<()>) {
        if let Err(error) = self.tasks.try_reserve(1) {
            task.abort();
            panic!("reserve panic-safe MCP task registration: {error}");
        }
        self.tasks.push(task);
    }

    fn track_barrier_release(&mut self, release: impl FnOnce() + Send + 'static) {
        self.barrier_releases.push(Box::new(release));
    }

    fn force_state_cleanup_error(&mut self, error: impl Into<String>) {
        self.forced_state_cleanup_error = Some(error.into());
    }

    fn force_path_cleanup_error(&mut self, path: PathBuf, error: impl Into<String>) {
        self.forced_path_cleanup_errors.insert(path, error.into());
    }

    fn report_cleanup_error_while_preserving_panic(&self, error: &str) {
        let diagnostic =
            format!("panic-safe MCP fixture cleanup also failed during unwind: {error}");
        eprintln!("{diagnostic}");
        if let Ok(mut diagnostics) = self.cleanup_diagnostics.lock() {
            diagnostics.push(diagnostic);
        }
    }

    fn spawn_worker<F>(&mut self, future: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.spawn_worker_with_probe(future).0
    }

    fn spawn_worker_with_probe<F>(
        &mut self,
        future: F,
    ) -> (
        tokio::task::JoinHandle<F::Output>,
        Arc<PanicSafeMcpWorkerState>,
    )
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.workers
            .try_reserve(1)
            .expect("reserve panic-safe MCP worker registration");
        let state = Arc::new(PanicSafeMcpWorkerState::new());
        let completion = PanicSafeMcpWorkerCompletion {
            state: state.clone(),
        };
        let task = tokio::spawn(async move {
            let _completion = completion;
            future.await
        });
        self.workers.push(PanicSafeMcpWorker {
            abort: task.abort_handle(),
            state: state.clone(),
        });
        (task, state)
    }

    fn release_barriers(&mut self) {
        if let Some(barrier) = MCP_HTTP_DESCRIPTOR_INSERT_BARRIER.get()
            && let Ok(mut barrier) = barrier.lock()
        {
            barrier.take();
        }
        for release in self.barrier_releases.drain(..).rev() {
            release();
        }
    }

    async fn cleanup(&mut self) -> Result<(), String> {
        let mut errors = Vec::new();
        self.release_barriers();
        for worker in &self.workers {
            worker.abort.abort();
        }
        for task in &self.tasks {
            task.abort();
        }

        for worker in self.workers.drain(..) {
            worker.state.wait().await;
        }
        let tasks = std::mem::take(&mut self.tasks);
        for task in tasks {
            match task.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => {
                    errors.push(format!("tracked MCP task failed during cleanup: {error}"));
                }
            }
        }

        clear_mcp_caches_for_test().await;
        if let Err(error) = assert_mcp_test_state_empty() {
            errors.push(error);
        }
        if let Some(error) = self.forced_state_cleanup_error.take() {
            errors.push(error);
        }

        let paths = std::mem::take(&mut self.paths);
        for path in paths.iter().rev() {
            match fs::remove_dir_all(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    errors.push(format!(
                        "failed to remove MCP test directory {}: {error}",
                        path.display()
                    ));
                    continue;
                }
            }
            match path.try_exists() {
                Ok(false) => {}
                Ok(true) => errors.push(format!(
                    "MCP test directory still exists after cleanup: {}",
                    path.display()
                )),
                Err(error) => errors.push(format!(
                    "failed to verify {} removal: {error}",
                    path.display()
                )),
            }
            if let Some(error) = self.forced_path_cleanup_errors.remove(path) {
                errors.push(error);
            }
        }
        for (_, error) in self.forced_path_cleanup_errors.drain() {
            errors.push(error);
        }
        self.cleaned = true;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

fn assert_mcp_test_state_empty() -> Result<(), String> {
    let runtime = http_runtime_state()
        .lock()
        .map_err(|_| "HTTP runtime state lock is poisoned".to_string())?;
    let runtime_empty = runtime.sessions.is_empty()
        && runtime.last_event_ids.is_empty()
        && runtime.stream_tasks.is_empty()
        && runtime.cleanups.is_empty()
        && runtime.cleanup_tasks.is_empty()
        && runtime.controls.is_empty()
        && runtime.remote_domains_by_server.is_empty()
        && runtime.remote_domain_by_cache_key.is_empty()
        && runtime.supersessions.is_empty()
        && runtime.in_flight_requests.is_empty()
        && runtime.deferred_cleanups.is_empty();
    drop(runtime);
    if !runtime_empty {
        return Err("HTTP MCP runtime state remains after cleanup".to_string());
    }
    if !session_cache()
        .lock()
        .map_err(|_| "MCP session cache lock is poisoned".to_string())?
        .is_empty()
    {
        return Err("MCP session cache remains after cleanup".to_string());
    }
    if !tool_cache()
        .lock()
        .map_err(|_| "MCP tool cache lock is poisoned".to_string())?
        .is_empty()
        || !resource_cache()
            .lock()
            .map_err(|_| "MCP resource cache lock is poisoned".to_string())?
            .is_empty()
        || !prompt_cache()
            .lock()
            .map_err(|_| "MCP prompt cache lock is poisoned".to_string())?
            .is_empty()
    {
        return Err("MCP descriptor cache remains after cleanup".to_string());
    }
    Ok(())
}

async fn finish_panic_safe_mcp_test(
    guard: tokio::sync::MutexGuard<'static, ()>,
    mut fixture: PanicSafeMcpFixture,
    body_result: std::thread::Result<()>,
) {
    let cleanup_result = fixture.cleanup().await;
    drop(guard);
    match (body_result, cleanup_result) {
        (Ok(()), Ok(())) => {}
        (Ok(()), Err(error)) => panic!("panic-safe MCP fixture cleanup failed: {error}"),
        (Err(payload), Ok(())) => std::panic::resume_unwind(payload),
        (Err(payload), Err(error)) => {
            fixture.report_cleanup_error_while_preserving_panic(&error);
            std::panic::resume_unwind(payload);
        }
    }
}

macro_rules! run_panic_safe_mcp_test {
    ($cleanup:ident, $body:block) => {{
        let guard = acquire_mcp_test_guard().await;
        clear_mcp_caches_for_test().await;
        let mut $cleanup = PanicSafeMcpFixture::default();
        let body_result = std::panic::AssertUnwindSafe(async $body)
            .catch_unwind()
            .await;
        finish_panic_safe_mcp_test(guard, $cleanup, body_result).await;
    }};
}

impl Drop for PanicSafeMcpFixture {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        self.release_barriers();
        for worker in &self.workers {
            worker.abort.abort();
        }
        if !self.workers.is_empty() {
            let message = "PanicSafeMcpFixture with managed workers requires async cleanup";
            if std::thread::panicking() {
                eprintln!("{message}");
            } else {
                panic!("{message}");
            }
            return;
        }
        if let Some(barrier) = MCP_HTTP_DESCRIPTOR_INSERT_BARRIER.get()
            && let Ok(mut barrier) = barrier.lock()
        {
            barrier.take();
        }
        if let Ok(mut state) = http_runtime_state().lock() {
            for (_, entry) in state.stream_tasks.drain() {
                entry.handle.abort();
            }
            for (_, entry) in state.cleanup_tasks.drain() {
                entry._handle.abort();
            }
            state.sessions.clear();
            state.last_event_ids.clear();
            state.cleanups.clear();
            state.controls.clear();
            state.remote_domains_by_server.clear();
            state.remote_domain_by_cache_key.clear();
            state.supersessions.clear();
            state.in_flight_requests.clear();
            state.deferred_cleanups.clear();
        }
        let sessions = session_cache()
            .lock()
            .map(|mut cache| {
                cache
                    .drain()
                    .map(|(_, entry)| entry.session)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for session in sessions {
            if let Ok(mut guard) = session.try_lock() {
                let _ = guard.child.start_kill();
                if let Some(task) = guard.stderr_task.take() {
                    task.abort();
                }
            }
        }
        for task in self.tasks.drain(..) {
            task.abort();
        }
        for path in self.paths.iter().rev() {
            if let Err(error) = fs::remove_dir_all(path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                if std::thread::panicking() {
                    eprintln!(
                        "failed to remove MCP test directory {} during unwind: {error}",
                        path.display()
                    );
                } else {
                    panic!(
                        "failed to remove MCP test directory {}: {error}",
                        path.display()
                    );
                }
            }
        }
    }
}

fn http_cleanup_phase(cache_key: &str) -> Option<HttpCleanupPhase> {
    http_runtime_state()
        .lock()
        .expect("HTTP runtime state lock")
        .cleanups
        .get(cache_key)
        .map(|cleanup| cleanup.phase)
}

fn seed_empty_http_descriptor_caches(cache_key: &str) {
    tool_cache().lock().expect("tool cache lock").insert(
        cache_key.to_string(),
        CachedToolDescriptors {
            descriptors: Vec::new(),
            loaded_at: Instant::now(),
            http_authority: None,
        },
    );
    resource_cache()
        .lock()
        .expect("resource cache lock")
        .insert(
            cache_key.to_string(),
            CachedResourceDescriptors {
                descriptors: Vec::new(),
                loaded_at: Instant::now(),
                http_authority: None,
            },
        );
    prompt_cache().lock().expect("prompt cache lock").insert(
        cache_key.to_string(),
        CachedPromptDescriptors {
            descriptors: Vec::new(),
            loaded_at: Instant::now(),
            http_authority: None,
        },
    );
}

fn assert_http_descriptor_caches_absent(cache_key: &str) {
    assert!(
        !tool_cache()
            .lock()
            .expect("tool cache lock")
            .contains_key(cache_key)
    );
    assert!(
        !resource_cache()
            .lock()
            .expect("resource cache lock")
            .contains_key(cache_key)
    );
    assert!(
        !prompt_cache()
            .lock()
            .expect("prompt cache lock")
            .contains_key(cache_key)
    );
}

async fn wait_for_http_cleanup_to_clear(cache_key: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while http_cleanup_phase(cache_key).is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("HTTP cleanup state should clear");
}

fn install_http_test_generation(
    cache_key: &str,
    session_id: &str,
    workspace_root: &CheckedWorkspacePath,
    event_id: &str,
) -> (HttpSessionIdentity, u64) {
    let identity = set_http_session_id(
        cache_key,
        Some(session_id.to_string()),
        workspace_root,
        true,
    )
    .expect("test HTTP session identity should install");
    set_http_last_event_id(cache_key, &identity, event_id);
    let task_id = next_http_stream_task_id();
    let handle = tokio::spawn(std::future::pending::<()>());
    if let Some(old) = http_stream_tasks()
        .lock()
        .expect("HTTP stream task lock")
        .insert(
            cache_key.to_string(),
            HttpStreamTaskEntry {
                task_id,
                epoch: identity.epoch,
                generation: identity.generation,
                handle,
            },
        )
    {
        old.handle.abort();
    }
    (identity, task_id)
}

fn assert_http_test_generation_is_current(
    cache_key: &str,
    identity: &HttpSessionIdentity,
    event_id: &str,
    task_id: u64,
) {
    assert!(
        http_session_identity_is_current(cache_key, identity),
        "new HTTP session generation must remain current"
    );
    assert_eq!(
        http_last_event_id(cache_key, identity).as_deref(),
        Some(event_id),
        "old cleanup must not remove the new generation event id"
    );
    let tasks = http_stream_tasks().lock().expect("HTTP stream task lock");
    let task = tasks
        .get(cache_key)
        .expect("new generation stream task must remain cached");
    assert_eq!(task.generation, identity.generation);
    assert_eq!(task.epoch, identity.epoch);
    assert_eq!(task.task_id, task_id);
    assert!(
        !task.handle.is_finished(),
        "old cleanup must not abort the new generation stream task"
    );
}

fn log_line_count(log_path: &Path, needle: &str) -> usize {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(needle))
        .count()
}

async fn wait_for_file_log_line(log_path: &Path, needle: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if fs::read_to_string(log_path)
                .unwrap_or_default()
                .contains(needle)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("mock stdio server should reach the requested log point");
}

async fn wait_for_http_method(log: &Arc<tokio::sync::Mutex<Vec<Value>>>, method: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if log.lock().await.iter().any(|call| call["method"] == method) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("mock HTTP server should receive the requested method");
}

#[test]
fn sanitize_name_segment_normalizes_non_identifier_chars() {
    assert_eq!(sanitize_name_segment("GitHub Repo"), "github_repo");
    assert_eq!(sanitize_name_segment("123-server"), "t_123_server");
    assert_eq!(sanitize_name_segment("---"), "tool");
}

#[test]
fn build_exposed_name_adds_suffix_for_collisions() {
    let first = build_exposed_name("github", "list issues");
    let second = build_exposed_name("github", "list-issues");

    assert!(first.starts_with("mcp__github__list_issues__"));
    assert!(second.starts_with("mcp__github__list_issues__"));
    assert_ne!(first, second);
}

#[test]
fn build_exposed_name_stays_unique_for_sanitized_server_collisions() {
    let first = build_exposed_name("github-repo", "list issues");
    let second = build_exposed_name("github_repo", "list issues");

    assert!(first.starts_with("mcp__github_repo__list_issues__"));
    assert!(second.starts_with("mcp__github_repo__list_issues__"));
    assert_ne!(first, second);
}

#[test]
fn render_call_result_prefers_text_and_structured_content() {
    let rendered = render_call_result(&json!({
        "content": [
            {"type": "text", "text": "hello"},
            {"type": "resource", "uri": "file:///tmp/demo"}
        ],
        "structuredContent": {"ok": true}
    }));

    assert!(rendered.output.contains("hello"));
    assert!(rendered.output.contains("[resource]"));
    assert!(rendered.output.contains("structuredContent"));
    assert!(rendered.images.is_empty());
}

#[test]
fn render_call_result_extracts_png_without_leaking_base64() {
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    png.extend_from_slice(&[
        0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0, 0, 0, 0, 0,
    ]);
    png.extend_from_slice(&[0, 0, 0, 1, b'I', b'D', b'A', b'T', 0, 0, 0, 0, 0]);
    png.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0, 0, 0, 0]);
    let encoded = STANDARD.encode(&png);
    let rendered = render_call_result(&json!({
        "content": [{"type":"image", "data": encoded, "mimeType":"image/png"}],
        "structuredContent": {"copy": encoded, "label": "keep me"}
    }));

    assert_eq!(rendered.images.len(), 1);
    assert_eq!(rendered.images[0].mime_type, "image/png");
    assert!(!rendered.output.contains(&encoded));
    assert!(rendered.output.contains("image output"));
    assert!(rendered.output.contains("[binary data omitted]"));
    assert!(rendered.output.contains("keep me"));
}

#[test]
fn render_call_result_redacts_known_image_payload_even_when_text_comes_first() {
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    png.extend_from_slice(&[
        0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0, 0, 0, 0, 0,
    ]);
    png.extend_from_slice(&[0, 0, 0, 1, b'I', b'D', b'A', b'T', 0, 0, 0, 0, 0]);
    png.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0, 0, 0, 0]);
    let encoded = STANDARD.encode(&png);
    let rendered = render_call_result(&json!({
        "content": [
            {"type":"text", "text":encoded},
            {"type":"image", "data":encoded, "mimeType":"image/png"}
        ]
    }));

    assert_eq!(rendered.images.len(), 1);
    assert!(!rendered.output.contains(&encoded));
    assert!(rendered.output.contains("[binary data omitted]"));
}

#[test]
fn render_call_result_redacts_known_image_payload_embedded_in_text() {
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    png.extend_from_slice(&[
        0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0, 0, 0, 0, 0,
    ]);
    png.extend_from_slice(&[0, 0, 0, 1, b'I', b'D', b'A', b'T', 0, 0, 0, 0, 0]);
    png.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0, 0, 0, 0]);
    let encoded = STANDARD.encode(&png);
    let rendered = render_call_result(&json!({
        "content": [
            {
                "type":"text",
                "text":format!("preview=data:image/png;base64,{encoded}; source=mcp")
            },
            {"type":"image", "data":encoded, "mimeType":"image/png"}
        ],
        "structuredContent": {
            "description": format!("embedded image: {encoded}")
        }
    }));

    assert_eq!(rendered.images.len(), 1);
    assert!(!rendered.output.contains(&encoded));
    assert!(
        rendered
            .output
            .contains("preview=data:image/png;base64,[binary data omitted]")
    );
    assert!(
        rendered
            .output
            .contains("embedded image: [binary data omitted]")
    );
}

#[test]
fn shared_tool_image_budget_limits_decoding_across_mcp_results() {
    let mut png = Vec::new();
    png.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    png.extend_from_slice(&[
        0, 0, 0, 13, b'I', b'H', b'D', b'R', 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0, 0, 0, 0, 0,
    ]);
    png.extend_from_slice(&[0, 0, 0, 1, b'I', b'D', b'A', b'T', 0, 0, 0, 0, 0]);
    png.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0, 0, 0, 0]);
    let encoded = STANDARD.encode(&png);
    let result = json!({
        "content": [{"type":"image", "data":encoded, "mimeType":"image/png"}]
    });
    let budget = ToolImageBudget::new(1);

    let first = render_call_result_with_image_budget(&result, Some(&budget));
    let second = render_call_result_with_image_budget(&result, Some(&budget));

    assert_eq!(first.images.len(), 1);
    assert!(second.images.is_empty());
    assert!(second.output.contains("tool image batch limit reached"));
}

#[tokio::test]
async fn tool_image_budget_reservations_follow_original_call_order() {
    let budget = ToolImageBudget::new(1);
    let first = budget.for_call(0);
    let second = budget.for_call(1);
    let (reserved_tx, mut reserved_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        second.wait_for_turn().await;
        let _ = reserved_tx.send(second.try_reserve());
    });

    assert!(
        tokio::time::timeout(Duration::from_millis(20), &mut reserved_rx)
            .await
            .is_err(),
        "a later call must wait even when it reaches image processing first"
    );
    first.wait_for_turn().await;
    assert!(first.try_reserve());
    drop(first);

    let second_reserved = tokio::time::timeout(Duration::from_secs(1), reserved_rx)
        .await
        .expect("later call should be released")
        .expect("reservation task should report a result");
    assert!(
        !second_reserved,
        "the earlier call must retain the only slot"
    );
}

#[test]
fn render_call_result_omits_audio_and_independent_structured_binary_payloads() {
    let audio = STANDARD.encode(b"RIFF-fake-audio-payload-that-must-not-be-serialized");
    let independent = STANDARD.encode(b"independent-structured-binary-payload");
    let orphan = STANDARD.encode(vec![0xAB; 128]);
    let rendered = render_call_result(&json!({
        "content": [{"type":"audio", "data":audio, "mimeType":"audio/wav"}],
        "structuredContent": {
            "recording": {"mimeType":"audio/wav", "data":independent},
            "orphanPayload":orphan,
            "label":"keep me"
        }
    }));

    assert!(rendered.images.is_empty());
    assert!(!rendered.output.contains(&audio));
    assert!(!rendered.output.contains(&independent));
    assert!(!rendered.output.contains(&orphan));
    assert!(rendered.output.contains("[binary data omitted]"));
    assert!(rendered.output.contains("keep me"));
}

#[test]
fn render_call_result_sanitizes_binary_payloads_in_fallback_results() {
    let encoded = STANDARD.encode(b"fallback-binary-payload-that-must-not-leak");
    let rendered = render_call_result(&json!({
        "result": {"encoding":"base64", "data":encoded}
    }));

    assert!(!rendered.output.contains(&encoded));
    assert!(rendered.output.contains("[binary data omitted]"));
}

#[test]
fn runtime_tool_note_lists_enabled_servers() {
    let workspace = unique_temp_workspace("lingclaw-mcp-runtime-note");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let mut config = test_config_with_mcp();
    config.mcp_servers.insert(
        "resources".to_string(),
        JsonMcpServerConfig {
            transport: None,
            command: "npx".to_string(),
            url: None,
            args: Vec::new(),
            env: HashMap::new(),
            headers: HashMap::new(),
            cwd: None,
            enabled: true,
            auth: None,
            timeout_secs: Some(20),
        },
    );
    assert!(runtime_tool_note(&config, &workspace).is_none());

    save_session_policy(
        &workspace,
        &McpSessionPolicy {
            enabled_servers: HashSet::from(["github".to_string(), "resources".to_string()]),
            enabled_tools: HashSet::from(["mcp__github__list_issues__abc12345".to_string()]),
            confirm_mutating_tools: false,
            client_capabilities: Default::default(),
            cache_namespace: None,
        },
    )
    .expect("MCP session policy should save");
    let note = runtime_tool_note(&config, &workspace).expect("note should exist");

    assert!(note.contains("github"));
    assert!(
        !note.contains("resources"),
        "servers with no enabled tools should not be advertised as MCP tool sources"
    );
    assert!(note.contains("mcp__"));
    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn parses_www_authenticate_resource_metadata() {
    let quoted = r#"Bearer resource_metadata="https://example.com/.well-known/oauth-protected-resource", scope="read""#;
    assert_eq!(
        parse_www_authenticate_metadata(quoted).as_deref(),
        Some("https://example.com/.well-known/oauth-protected-resource")
    );

    let unquoted = "Bearer realm=test, resource_metadata=https://example.com/meta";
    assert_eq!(
        parse_www_authenticate_metadata(unquoted).as_deref(),
        Some("https://example.com/meta")
    );
}

#[tokio::test]
async fn sse_parser_tracks_last_event_id_and_invalidates_caches_for_notifications() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;
    let cache_key = "sse-cache-key";
    let workspace = unique_temp_workspace("lingclaw-sse-event-id");
    fs::create_dir_all(&workspace).expect("create SSE workspace");
    let workspace_root = resolve_path_checked(".", &workspace).expect("resolve SSE workspace");
    let identity = set_http_session_id(
        cache_key,
        Some("sse-session".to_string()),
        &workspace_root,
        true,
    )
    .expect("install SSE session identity");
    {
        let mut cache = tool_cache().lock().expect("tool cache lock");
        cache.insert(
            cache_key.to_string(),
            CachedToolDescriptors {
                descriptors: Vec::new(),
                loaded_at: Instant::now(),
                http_authority: None,
            },
        );
    }

    let response = parse_sse_json_response(
        r#"id: 41
event: message
data: {"jsonrpc":"2.0","method":"notifications/tools/list_changed","params":{}}

id: 42
event: message
data: {"jsonrpc":"2.0","id":7,"result":{"tools":[]}}

"#,
        cache_key,
    )
    .expect("SSE response should parse");

    assert_eq!(response["id"], 7);
    assert_eq!(
        http_last_event_id(cache_key, &identity).as_deref(),
        Some("42")
    );
    assert!(
        !tool_cache()
            .lock()
            .expect("tool cache lock")
            .contains_key(cache_key)
    );

    remove_http_session(cache_key);
    drop(workspace_root);
    fs::remove_dir_all(workspace).expect("clean SSE workspace");
}

#[tokio::test]
async fn expired_http_session_is_reported_without_silent_local_removal() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let cache_key = "expired-http-session";
    let workspace = unique_temp_workspace("lingclaw-expired-http-session");
    fs::create_dir_all(&workspace).expect("create HTTP session workspace");
    let workspace_root =
        resolve_path_checked(".", &workspace).expect("resolve HTTP session workspace");
    let identity = set_http_session_id(
        cache_key,
        Some("old-session".to_string()),
        &workspace_root,
        true,
    )
    .expect("install expiring HTTP session");
    if let Ok(mut cache) = http_session_cache().lock() {
        cache
            .get_mut(cache_key)
            .expect("expiring HTTP session should be cached")
            .last_used_at = Instant::now() - session_idle_ttl() - Duration::from_secs(1);
    }
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    if let Ok(mut tasks) = http_stream_tasks().lock() {
        tasks.insert(
            cache_key.to_string(),
            HttpStreamTaskEntry {
                task_id: next_http_stream_task_id(),
                epoch: identity.epoch,
                generation: identity.generation,
                handle: tokio::spawn(async move {
                    let _ = rx.await;
                }),
            },
        );
    }

    match http_session_lookup(cache_key).expect("inspect expired HTTP Session") {
        HttpSessionLookup::Expired(expired) => assert_eq!(expired, identity),
        _ => panic!("idle Session must be returned to the asynchronous cleanup path"),
    }
    assert!(
        http_stream_tasks()
            .lock()
            .expect("HTTP stream tasks lock")
            .contains_key(cache_key),
        "the synchronous lookup must not silently orphan the remote Session or its stream"
    );

    remove_http_session(cache_key);
    drop(workspace_root);
    clear_mcp_caches_for_test().await;
    fs::remove_dir_all(workspace).expect("clean HTTP session workspace");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn event_stream_workspace_invalidation_cleanup_preserves_a_new_http_session_generation() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let fixture = unique_temp_workspace("lingclaw-http-invalidation-generation");
    cleanup.track_path(fixture.clone());
    let workspace = fixture.join("workspace");
    let moved = fixture.join("workspace-old");
    fs::create_dir_all(&workspace).expect("create old HTTP workspace");
    let (url, state, server_task) = spawn_delayed_delete_streamable_http_test_server().await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .get("http")
        .expect("HTTP test server config")
        .clone();
    let cache_key = "http-generation-workspace-invalidation";
    let old_root = resolve_path_checked("workspace", &fixture).expect("resolve old HTTP workspace");
    let old_identity =
        set_http_session_id(cache_key, Some("old-session".to_string()), &old_root, true)
            .expect("install old HTTP stream generation");
    set_http_last_event_id(cache_key, &old_identity, "old-event");

    fs::rename(&workspace, &moved).expect("move old HTTP workspace");
    fs::create_dir(&workspace).expect("install new HTTP workspace namespace");
    start_http_event_stream(
        "http",
        &server,
        cache_key,
        &old_identity.session_id,
        &old_root,
        &McpClientCapabilityPolicy::default(),
        5,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), state.delete_started.notified())
        .await
        .expect("old event-stream cleanup should reach the DELETE barrier");

    let new_root = resolve_path_checked("workspace", &fixture).expect("resolve new HTTP workspace");
    let new_identity =
        set_http_session_id(cache_key, Some("new-session".to_string()), &new_root, true)
            .expect("install replacement HTTP stream generation");
    set_http_last_event_id(cache_key, &new_identity, "new-event");
    let new_task_id = next_http_stream_task_id();
    let replacement_handle = tokio::spawn(std::future::pending::<()>());
    let replaced_stream = http_stream_tasks()
        .lock()
        .expect("HTTP stream task lock")
        .insert(
            cache_key.to_string(),
            HttpStreamTaskEntry {
                task_id: new_task_id,
                epoch: new_identity.epoch,
                generation: new_identity.generation,
                handle: replacement_handle,
            },
        );
    assert!(
        replaced_stream.is_none(),
        "cleanup ownership should detach the invalidated stream before DELETE completes"
    );
    assert_ne!(old_identity.generation, new_identity.generation);
    state.release_delete.notify_one();

    wait_for_http_cleanup_to_clear(cache_key).await;
    assert_http_test_generation_is_current(cache_key, &new_identity, "new-event", new_task_id);
    let calls = state.log.lock().await.clone();
    assert!(
        calls
            .iter()
            .any(|call| call["sessionId"] == old_identity.session_id),
        "the remote cleanup must target only the old session: {calls:?}"
    );

    remove_http_session(cache_key);
    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn awaited_http_termination_preserves_a_new_session_event_and_stream_generation() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-terminate-generation");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create HTTP generation workspace");
        let (url, state, server_task) = spawn_delayed_delete_streamable_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let state = state.clone();
            move || state.release_delete.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("HTTP test server config")
            .clone();
        let cache_key = "http-generation-explicit-termination";
        let workspace_root =
            resolve_path_checked(".", &workspace).expect("resolve HTTP generation workspace");
        let (old_identity, _) =
            install_http_test_generation(cache_key, "old-session", &workspace_root, "old-event");

        let cleanup_key = cache_key.to_string();
        let cleanup_server = server.clone();
        let cleanup_task = cleanup.spawn_worker(async move {
            terminate_http_session("http", &cleanup_key, &cleanup_server).await;
        });
        tokio::time::timeout(Duration::from_secs(2), state.delete_started.notified())
            .await
            .expect("old explicit cleanup should reach the DELETE barrier");

        let (new_identity, new_task_id) =
            install_http_test_generation(cache_key, "new-session", &workspace_root, "new-event");
        assert_ne!(old_identity.generation, new_identity.generation);
        state.release_delete.notify_one();
        cleanup_task.await.expect("join old explicit cleanup");

        assert_http_test_generation_is_current(cache_key, &new_identity, "new-event", new_task_id);
        let calls = state.log.lock().await.clone();
        assert!(
            calls
                .iter()
                .any(|call| call["sessionId"] == old_identity.session_id),
            "the awaited DELETE must be bound to the old session: {calls:?}"
        );

        remove_http_session(cache_key);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_initialize_response_cannot_reinstall_after_runtime_clear() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-delayed-initialize-epoch");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create delayed initialize workspace");
        let (url, server_state, server_task) =
            spawn_delayed_epoch_streamable_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_initialize.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("HTTP test server config")
            .clone();
        let workspace_root =
            resolve_path_checked(".", &workspace).expect("resolve delayed initialize workspace");
        let cache_key = "http\ndelayed-initialize-epoch".to_string();
        let request = cleanup.spawn_worker({
            let server = server.clone();
            let workspace_root = workspace_root.clone();
            let cache_key = cache_key.clone();
            async move {
                http_post_json(
                    "http",
                    &server,
                    &cache_key,
                    &workspace_root,
                    &McpClientCapabilityPolicy::default(),
                    json!({
                        "jsonrpc": "2.0",
                        "id": 41,
                        "method": "initialize",
                        "params": {}
                    }),
                    None,
                    5,
                )
                .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.initialize_started.notified(),
        )
        .await
        .expect("initialize request should reach the response barrier");
        let request_epoch = http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock")
            .controls[&cache_key]
            .epoch
            .load(Ordering::Acquire);

        clear_cached_runtime_state_for_server("http");
        let cleared_epoch = http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock")
            .controls[&cache_key]
            .epoch
            .load(Ordering::Acquire);
        assert_ne!(
            request_epoch, cleared_epoch,
            "clear must advance the epoch tombstone"
        );
        server_state.release_initialize.notify_one();

        let error = request
            .await
            .expect("join delayed initialize request")
            .expect_err("the cleared initialize response must be rejected");
        assert!(error.contains("invalidated request epoch"), "{error}");
        assert!(
            !http_session_cache()
                .lock()
                .expect("HTTP session cache lock")
                .contains_key(&cache_key),
            "a late initialize response must not recreate the cleared session"
        );
        assert!(
            server_state
                .deleted_sessions
                .lock()
                .await
                .iter()
                .any(|session_id| session_id == "late-initialize-session"),
            "the rejected remote session should be terminated"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delayed_normal_response_cannot_overwrite_a_rebuilt_http_session_generation() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-delayed-normal-epoch");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create delayed normal workspace");
        let (url, server_state, server_task) =
            spawn_delayed_epoch_streamable_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_normal.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("HTTP test server config")
            .clone();
        let workspace_root =
            resolve_path_checked(".", &workspace).expect("resolve delayed normal workspace");
        let cache_key = "http\ndelayed-normal-epoch".to_string();
        let (old_identity, _) = install_http_test_generation(
            &cache_key,
            "old-normal-session",
            &workspace_root,
            "old-event",
        );
        let request = cleanup.spawn_worker({
            let server = server.clone();
            let workspace_root = workspace_root.clone();
            let cache_key = cache_key.clone();
            async move {
                http_post_json(
                    "http",
                    &server,
                    &cache_key,
                    &workspace_root,
                    &McpClientCapabilityPolicy::default(),
                    json!({
                        "jsonrpc": "2.0",
                        "id": 42,
                        "method": "tools/list",
                        "params": {}
                    }),
                    Some("old-normal-session".to_string()),
                    5,
                )
                .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.normal_started.notified(),
        )
        .await
        .expect("normal request should reach the response barrier");

        let (new_identity, new_task_id) = install_http_test_generation(
            &cache_key,
            "new-normal-session",
            &workspace_root,
            "new-event",
        );
        assert_ne!(old_identity.epoch, new_identity.epoch);
        assert_ne!(old_identity.generation, new_identity.generation);
        server_state.release_normal.notify_one();

        let error = request
            .await
            .expect("join delayed normal request")
            .expect_err("the old normal response must be rejected");
        assert!(error.contains("invalidated request epoch"), "{error}");
        assert_http_test_generation_is_current(&cache_key, &new_identity, "new-event", new_task_id);
        assert!(
            server_state
                .deleted_sessions
                .lock()
                .await
                .iter()
                .any(|session_id| session_id == "old-normal-session"),
            "the stale remote session should be terminated without touching the replacement"
        );

        remove_http_session(&cache_key);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_invalidation_rejects_delayed_initialize_and_serializes_replacement() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-settings-invalidate");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create settings invalidation workspace");
        let (url, server_state, server_task) = spawn_settings_invalidate_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_first_initialize.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("HTTP test server config")
            .clone();
        let workspace_root =
            resolve_path_checked(".", &workspace).expect("resolve settings invalidation workspace");
        let cache_key = "http\nsettings-invalidation-authority".to_string();
        let first = cleanup.spawn_worker({
            let server = server.clone();
            let workspace_root = workspace_root.clone();
            let cache_key = cache_key.clone();
            async move {
                http_post_json(
                    "http",
                    &server,
                    &cache_key,
                    &workspace_root,
                    &McpClientCapabilityPolicy::default(),
                    json!({
                        "jsonrpc": "2.0",
                        "id": 51,
                        "method": "initialize",
                        "params": {}
                    }),
                    None,
                    5,
                )
                .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.first_initialize_started.notified(),
        )
        .await
        .expect("old initialize should reach the response barrier");
        let old_epoch = http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock")
            .controls[&cache_key]
            .epoch
            .load(Ordering::Acquire);

        invalidate_runtime_state_without_remote_shutdown().await;
        let invalidated_epoch = http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock")
            .controls[&cache_key]
            .epoch
            .load(Ordering::Acquire);
        assert_ne!(old_epoch, invalidated_epoch);

        let blocked_replacement = cleanup.spawn_worker({
            let server = server.clone();
            let workspace_root = workspace_root.clone();
            let cache_key = cache_key.clone();
            async move {
                http_post_json(
                    "http",
                    &server,
                    &cache_key,
                    &workspace_root,
                    &McpClientCapabilityPolicy::default(),
                    json!({
                        "jsonrpc": "2.0",
                        "id": 52,
                        "method": "initialize",
                        "params": {}
                    }),
                    None,
                    5,
                )
                .await
            }
        });
        let blocked_error = tokio::time::timeout(Duration::from_secs(2), blocked_replacement)
            .await
            .expect("replacement initialize must fail closed while old authority is unresolved")
            .expect("join blocked replacement initialize")
            .expect_err("replacement initialize must observe endpoint quarantine");
        assert!(
            blocked_error.contains("cleanup is unconfirmed"),
            "{blocked_error}"
        );
        assert_eq!(
            server_state.initialize_count.load(Ordering::Acquire),
            1,
            "the replacement config must not send initialize under the old authority"
        );

        server_state.release_first_initialize.notify_one();
        let first_error = first
            .await
            .expect("join old initialize")
            .expect_err("Settings invalidation must reject the old initialize response");
        assert!(
            first_error.contains("invalidated request epoch"),
            "{first_error}"
        );
        http_post_json(
            "http",
            &server,
            &cache_key,
            &workspace_root,
            &McpClientCapabilityPolicy::default(),
            json!({
                "jsonrpc": "2.0",
                "id": 53,
                "method": "initialize",
                "params": {}
            }),
            None,
            5,
        )
        .await
        .expect("confirmed cleanup should permit a fresh replacement initialize");

        assert_eq!(server_state.initialize_count.load(Ordering::Acquire), 2);
        let current = cached_http_session_identity_unchecked(&cache_key)
            .expect("replacement HTTP session should be current");
        assert_eq!(current.session_id, "settings-new-session");
        assert!(
            server_state
                .deleted_sessions
                .lock()
                .await
                .iter()
                .any(|session_id| session_id == "settings-old-session"),
            "the rejected old remote session should be cleaned up"
        );

        remove_http_session(&cache_key);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_delete_is_serialized_before_same_id_reuse() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-same-id-reuse");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create same-ID workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || {
                server_state.release_old_response.notify_waiters();
                server_state.release_delete.notify_waiters();
            }
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("HTTP test server config")
            .clone();
        let workspace_root =
            resolve_path_checked(".", &workspace).expect("resolve same-ID workspace");
        let cache_key = "http\nsame-id-reuse".to_string();
        set_http_session_id(
            &cache_key,
            Some("fixed-session-id".to_string()),
            &workspace_root,
            true,
        )
        .expect("install old fixed-ID session");

        let old_request = cleanup.spawn_worker({
            let server = server.clone();
            let workspace_root = workspace_root.clone();
            let cache_key = cache_key.clone();
            async move {
                http_post_json(
                    "http",
                    &server,
                    &cache_key,
                    &workspace_root,
                    &McpClientCapabilityPolicy::default(),
                    json!({
                        "jsonrpc": "2.0",
                        "id": 61,
                        "method": "tools/list",
                        "params": {}
                    }),
                    Some("fixed-session-id".to_string()),
                    5,
                )
                .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.old_response_started.notified(),
        )
        .await
        .expect("old response should reach its barrier");
        clear_cached_runtime_state_for_server("http");
        server_state.release_old_response.notify_one();
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("stale cleanup should reach the DELETE barrier");

        let pending_error = http_post_json(
            "http",
            &server,
            &cache_key,
            &workspace_root,
            &McpClientCapabilityPolicy::default(),
            json!({
                "jsonrpc": "2.0",
                "id": 62,
                "method": "initialize",
                "params": {}
            }),
            None,
            5,
        )
        .await
        .expect_err("same-ID replacement must be rejected while DELETE is pending");
        assert_eq!(pending_error, HTTP_MCP_CLEANUP_UNCERTAIN_ERROR);
        assert_eq!(
            server_state.active_generation.load(Ordering::Acquire),
            1,
            "the replacement generation must not install before old DELETE completes"
        );

        server_state.release_delete.notify_one();
        let old_error = old_request
            .await
            .expect("join stale normal request")
            .expect_err("stale normal response must be rejected");
        assert!(
            old_error.contains("invalidated request epoch"),
            "{old_error}"
        );
        http_post_json(
            "http",
            &server,
            &cache_key,
            &workspace_root,
            &McpClientCapabilityPolicy::default(),
            json!({
                "jsonrpc": "2.0",
                "id": 63,
                "method": "initialize",
                "params": {}
            }),
            None,
            5,
        )
        .await
        .expect("same-ID replacement initialize should succeed after confirmed DELETE");
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.initialize_started.notified(),
        )
        .await
        .expect("replacement initialize should reach the fixed-ID server");

        assert_eq!(
            server_state.deleted_generations.lock().await.as_slice(),
            &[1],
            "the delayed DELETE must affect only the old remote generation"
        );
        assert_eq!(server_state.active_generation.load(Ordering::Acquire), 2);
        let current = cached_http_session_identity_unchecked(&cache_key)
            .expect("same-ID replacement should remain cached");
        assert_eq!(current.session_id, "fixed-session-id");

        remove_http_session(&cache_key);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ambiguous_delete_blocks_same_id_reinitialization_after_client_timeout() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-ambiguous-delete-timeout");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create ambiguous DELETE workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_delete.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("HTTP test server config")
            .clone();
        let workspace_root =
            resolve_path_checked(".", &workspace).expect("resolve ambiguous DELETE workspace");
        let cache_key = "http\nambiguous-delete-timeout".to_string();
        set_http_session_id(
            &cache_key,
            Some("fixed-session-id".to_string()),
            &workspace_root,
            true,
        )
        .expect("install old fixed-ID session");

        let cleanup_task = cleanup.spawn_worker({
            let server = server.clone();
            let cache_key = cache_key.clone();
            async move {
                terminate_http_session("http", &cache_key, &server).await;
            }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("DELETE should reach the server");
        tokio::time::timeout(Duration::from_secs(4), cleanup_task)
            .await
            .expect("client DELETE timeout should be bounded")
            .expect("join DELETE cleanup caller");
        assert_eq!(
            http_cleanup_phase(&cache_key),
            Some(HttpCleanupPhase::Uncertain)
        );
        assert!(
            !http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock")
                .cleanup_tasks
                .contains_key(&cache_key),
            "the bounded background request should leave only the fail-closed tombstone"
        );

        let error = http_post_json(
            "http",
            &server,
            &cache_key,
            &workspace_root,
            &McpClientCapabilityPolicy::default(),
            json!({
                "jsonrpc": "2.0",
                "id": 63,
                "method": "initialize",
                "params": {}
            }),
            None,
            5,
        )
        .await
        .expect_err("an unconfirmed DELETE must isolate the cache key");
        assert_eq!(error, HTTP_MCP_CLEANUP_UNCERTAIN_ERROR);
        assert_eq!(server_state.active_generation.load(Ordering::Acquire), 1);

        let delete_finished = server_state.delete_finished.notified();
        server_state.release_delete.notify_one();
        tokio::time::timeout(Duration::from_secs(2), delete_finished)
            .await
            .expect("the server should eventually apply the timed-out DELETE");
        assert_eq!(server_state.active_generation.load(Ordering::Acquire), 0);
        assert_eq!(
            http_cleanup_phase(&cache_key),
            Some(HttpCleanupPhase::Uncertain),
            "a late server-side DELETE cannot retroactively confirm the client outcome"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_delete_caller_retains_cleanup_ownership_and_blocks_reinitialization() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-ambiguous-delete-cancel");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create cancelled DELETE workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_delete.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("HTTP test server config")
            .clone();
        let workspace_root =
            resolve_path_checked(".", &workspace).expect("resolve cancelled DELETE workspace");
        let cache_key = "http\nambiguous-delete-cancel".to_string();
        set_http_session_id(
            &cache_key,
            Some("fixed-session-id".to_string()),
            &workspace_root,
            true,
        )
        .expect("install old fixed-ID session");

        let cleanup_caller = cleanup.spawn_worker({
            let server = server.clone();
            let cache_key = cache_key.clone();
            async move { terminate_http_session("http", &cache_key, &server).await }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("DELETE should reach the cancellation barrier");
        cleanup_caller.abort();
        let join_error = cleanup_caller
            .await
            .expect_err("the cleanup caller should be cancelled");
        assert!(join_error.is_cancelled());
        assert_eq!(
            http_cleanup_phase(&cache_key),
            Some(HttpCleanupPhase::Uncertain),
            "caller cancellation must transfer the pending request to uncertain ownership"
        );
        assert!(
            http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock")
                .cleanup_tasks
                .contains_key(&cache_key),
            "the runtime-owned DELETE must outlive its cancelled caller"
        );

        let error = http_post_json(
            "http",
            &server,
            &cache_key,
            &workspace_root,
            &McpClientCapabilityPolicy::default(),
            json!({
                "jsonrpc": "2.0",
                "id": 64,
                "method": "initialize",
                "params": {}
            }),
            None,
            5,
        )
        .await
        .expect_err("a cancelled cleanup caller must not release the cache key");
        assert_eq!(error, HTTP_MCP_CLEANUP_UNCERTAIN_ERROR);
        assert_eq!(server_state.active_generation.load(Ordering::Acquire), 1);

        let delete_finished = server_state.delete_finished.notified();
        server_state.release_delete.notify_one();
        tokio::time::timeout(Duration::from_secs(2), delete_finished)
            .await
            .expect("the runtime-owned DELETE should finish after caller cancellation");
        wait_for_http_cleanup_to_clear(&cache_key).await;
        assert_eq!(server_state.active_generation.load(Ordering::Acquire), 0);
        assert!(
            !http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock")
                .cleanup_tasks
                .contains_key(&cache_key)
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirmed_delete_releases_its_key_without_blocking_an_independent_key() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-confirmed-delete");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create confirmed DELETE workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_delete.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("HTTP fixed-ID server config")
            .clone();
        let (independent_url, independent_log, independent_task) =
            spawn_auth_recording_streamable_http_test_server().await;
        cleanup.track_task(independent_task);
        let independent_config = test_config_with_streamable_http_server(independent_url);
        let independent_server = independent_config
            .mcp_servers
            .get("http")
            .expect("independent HTTP server config")
            .clone();
        let workspace_root =
            resolve_path_checked(".", &workspace).expect("resolve confirmed DELETE workspace");
        let cache_key = "http\nconfirmed-delete".to_string();
        let independent_key = "http\nindependent-during-delete".to_string();
        set_http_session_id(
            &cache_key,
            Some("fixed-session-id".to_string()),
            &workspace_root,
            true,
        )
        .expect("install old fixed-ID session");

        let cleanup_caller = cleanup.spawn_worker({
            let server = server.clone();
            let cache_key = cache_key.clone();
            async move { terminate_http_session("http", &cache_key, &server).await }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("confirmed DELETE should reach its barrier");

        http_post_json(
            "http",
            &independent_server,
            &independent_key,
            &workspace_root,
            &McpClientCapabilityPolicy::default(),
            json!({
                "jsonrpc": "2.0",
                "id": 65,
                "method": "initialize",
                "params": {}
            }),
            None,
            5,
        )
        .await
        .expect("an independent cache key must initialize while DELETE is pending");
        assert!(
            independent_log
                .lock()
                .await
                .iter()
                .any(|entry| { entry.get("method").and_then(Value::as_str) == Some("initialize") })
        );
        assert_eq!(server_state.active_generation.load(Ordering::Acquire), 1);

        server_state.release_delete.notify_one();
        let outcome = cleanup_caller.await.expect("join confirmed cleanup caller");
        assert_eq!(outcome, HttpDeleteOutcome::Confirmed);
        assert_eq!(http_cleanup_phase(&cache_key), None);

        http_post_json(
            "http",
            &server,
            &cache_key,
            &workspace_root,
            &McpClientCapabilityPolicy::default(),
            json!({
                "jsonrpc": "2.0",
                "id": 66,
                "method": "initialize",
                "params": {}
            }),
            None,
            5,
        )
        .await
        .expect("confirmed DELETE must allow same-key reinitialization");
        assert_eq!(server_state.active_generation.load(Ordering::Acquire), 2);
        assert_eq!(
            cached_http_session_identity_unchecked(&cache_key)
                .expect("replacement fixed-ID session")
                .session_id,
            "fixed-session-id"
        );

        remove_http_session(&cache_key);
        remove_http_session(&independent_key);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_delete_keeps_the_cache_key_quarantined_before_deferred_side_effect() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-accepted-delete");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create accepted DELETE workspace");
    let (url, server_state, server_task) =
        spawn_same_id_reuse_http_test_server_with_delete(StatusCode::ACCEPTED, true).await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .get("http")
        .expect("HTTP accepted DELETE server config")
        .clone();
    let workspace_root =
        resolve_path_checked(".", &workspace).expect("resolve accepted DELETE workspace");
    let cache_key = "http\naccepted-delete".to_string();
    set_http_session_id(
        &cache_key,
        Some("fixed-session-id".to_string()),
        &workspace_root,
        true,
    )
    .expect("install fixed-ID session before accepted DELETE");

    let outcome = terminate_http_session("http", &cache_key, &server).await;
    assert_eq!(outcome, HttpDeleteOutcome::Ambiguous);
    assert_eq!(
        http_cleanup_phase(&cache_key),
        Some(HttpCleanupPhase::Uncertain),
        "202 Accepted must not clear the cleanup tombstone"
    );
    let error = http_post_json(
        "http",
        &server,
        &cache_key,
        &workspace_root,
        &McpClientCapabilityPolicy::default(),
        json!({
            "jsonrpc": "2.0",
            "id": 67,
            "method": "initialize",
            "params": {}
        }),
        None,
        5,
    )
    .await
    .expect_err("a deferred accepted DELETE must block same-key reinitialization");
    assert_eq!(error, HTTP_MCP_CLEANUP_UNCERTAIN_ERROR);
    assert_eq!(server_state.active_generation.load(Ordering::Acquire), 1);

    let delete_finished = server_state.delete_finished.notified();
    server_state.release_delete.notify_one();
    tokio::time::timeout(Duration::from_secs(2), delete_finished)
        .await
        .expect("the accepted DELETE side effect should eventually run");
    assert_eq!(server_state.active_generation.load(Ordering::Acquire), 0);
    assert_eq!(
        http_cleanup_phase(&cache_key),
        Some(HttpCleanupPhase::Uncertain),
        "a deferred side effect cannot retroactively prove client-visible completion"
    );

    clear_mcp_caches_for_test().await;
}

#[test]
fn delete_status_classification_requires_a_terminal_http_semantic() {
    assert_eq!(
        classify_http_delete_status(StatusCode::OK),
        HttpDeleteOutcome::Confirmed
    );
    assert_eq!(
        classify_http_delete_status(StatusCode::NO_CONTENT),
        HttpDeleteOutcome::Confirmed
    );
    for status in [
        StatusCode::NOT_FOUND,
        StatusCode::METHOD_NOT_ALLOWED,
        StatusCode::GONE,
    ] {
        assert_eq!(
            classify_http_delete_status(status),
            HttpDeleteOutcome::NotApplied
        );
    }
    for status in [
        StatusCode::ACCEPTED,
        StatusCode::CREATED,
        StatusCode::NON_AUTHORITATIVE_INFORMATION,
        StatusCode::RESET_CONTENT,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        assert_eq!(
            classify_http_delete_status(status),
            HttpDeleteOutcome::Ambiguous,
            "{status} must not release a cleanup quarantine"
        );
    }
}

#[test]
fn remote_cleanup_domain_tracks_only_the_normalized_http_endpoint() {
    let mut server = JsonMcpServerConfig {
        transport: Some("streamable-http".to_string()),
        command: "ignored".to_string(),
        url: Some("HTTP://127.0.0.1:8080/mcp#first".to_string()),
        args: vec!["ignored".to_string()],
        env: HashMap::from([("IGNORED".to_string(), "one".to_string())]),
        headers: HashMap::from([("x-api-key".to_string(), "one".to_string())]),
        cwd: Some(".".to_string()),
        enabled: true,
        auth: None,
        timeout_secs: Some(1),
    };
    let original = http_remote_cleanup_domain_key(&server).expect("original cleanup domain");

    server.command = "changed".to_string();
    server.args = vec!["changed".to_string()];
    server.env.insert("IGNORED".to_string(), "two".to_string());
    server
        .headers
        .insert("x-api-key".to_string(), "two".to_string());
    server.cwd = Some("changed".to_string());
    server.timeout_secs = Some(99);
    server.url = Some("http://127.0.0.1:8080/mcp#second".to_string());
    assert_eq!(
        http_remote_cleanup_domain_key(&server).expect("same normalized endpoint domain"),
        original
    );

    server.url = Some("http://127.0.0.1:8080/other".to_string());
    assert_ne!(
        http_remote_cleanup_domain_key(&server).expect("different endpoint domain"),
        original
    );

    server.url = Some("http://first:secret@127.0.0.1:8080/mcp#userinfo".to_string());
    let with_userinfo = http_remote_cleanup_domain_key(&server).expect("userinfo cleanup domain");
    server.url = Some("http://second:rotated@127.0.0.1:8080/mcp".to_string());
    assert_eq!(
        http_remote_cleanup_domain_key(&server).expect("rotated userinfo cleanup domain"),
        with_userinfo,
        "URL userinfo is authentication metadata, not part of the remote request target"
    );

    server.url = Some("http://EXAMPLE.com:80/a/../mcp?mode=one#local".to_string());
    let normalized_default_port =
        http_remote_cleanup_domain_key(&server).expect("default-port cleanup domain");
    server.url = Some("http://example.com/mcp?mode=one".to_string());
    assert_eq!(
        http_remote_cleanup_domain_key(&server).expect("canonical default-port cleanup domain"),
        normalized_default_port
    );
    server.url = Some("http://example.com/mcp?mode=two".to_string());
    assert_ne!(
        http_remote_cleanup_domain_key(&server).expect("different-query cleanup domain"),
        normalized_default_port,
        "query is part of the authoritative request target"
    );
    server.url = Some("http://example.com/mcp?marker=~".to_string());
    let unreserved_endpoint =
        http_remote_cleanup_domain_key(&server).expect("literal unreserved endpoint");
    for alias in [
        "http://example.com/%6dcp?marker=%7e",
        "http://example.com/%6Dcp?marker=%7E",
    ] {
        server.url = Some(alias.to_string());
        assert_eq!(
            http_remote_cleanup_domain_key(&server).expect("encoded unreserved endpoint"),
            unreserved_endpoint,
            "unreserved percent-encoding aliases must share one cleanup domain"
        );
    }
    server.url = Some("http://example.com/a%2fb".to_string());
    let encoded_separator =
        http_remote_cleanup_domain_key(&server).expect("encoded path separator endpoint");
    server.url = Some("http://example.com/a%2Fb".to_string());
    assert_eq!(
        http_remote_cleanup_domain_key(&server).expect("uppercase encoded separator endpoint"),
        encoded_separator,
        "reserved percent escapes should normalize hex case only"
    );
    server.url = Some("http://example.com/a/b".to_string());
    assert_ne!(
        http_remote_cleanup_domain_key(&server).expect("literal path separator endpoint"),
        encoded_separator,
        "a reserved encoded separator is not equivalent to a literal separator"
    );
    assert!(
        streamable_http_endpoint_url(&server)
            .expect("canonical reserved endpoint")
            .as_str()
            .contains("/a/b"),
        "the transport must use the same literal reserved-character boundary as the domain"
    );
    server.url = Some("http://example.com/a%2fb".to_string());
    assert!(
        streamable_http_endpoint_url(&server)
            .expect("canonical encoded separator endpoint")
            .as_str()
            .contains("/a%2Fb"),
        "canonicalization must not decode or double-encode a reserved separator"
    );
    server.url = Some("http://example.com/%E8%B7%AF%E5%BE%84?label=%C3%A9".to_string());
    let encoded_unicode =
        http_remote_cleanup_domain_key(&server).expect("encoded Unicode endpoint");
    server.url = Some("http://example.com/路径?label=é".to_string());
    assert_eq!(
        http_remote_cleanup_domain_key(&server).expect("literal Unicode endpoint"),
        encoded_unicode,
        "URL parsing and endpoint canonicalization must preserve UTF-8 octet semantics"
    );
    server.url = Some("http://example.com/mcp?invalid=%GG".to_string());
    assert!(
        http_remote_cleanup_domain_key(&server).is_err(),
        "invalid percent escapes must fail closed"
    );
    server.url = Some("http://[0:0:0:0:0:0:0:1]:80/mcp".to_string());
    let expanded_ipv6 =
        http_remote_cleanup_domain_key(&server).expect("expanded IPv6 cleanup domain");
    server.url = Some("http://[::1]/mcp".to_string());
    assert_eq!(
        http_remote_cleanup_domain_key(&server).expect("compressed IPv6 cleanup domain"),
        expanded_ipv6
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn percent_encoded_unreserved_aliases_cannot_bypass_endpoint_quarantine() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    for (canonical_suffix, alias_suffix) in
        [("/mcp", "/%6dcp"), ("/mcp?marker=~", "/mcp?marker=%7E")]
    {
        let (origin, server_state, server_task) =
            spawn_same_id_reuse_http_test_server_at_mcp(StatusCode::ACCEPTED, true).await;
        cleanup.track_task(server_task);
        let workspace = unique_temp_workspace("lingclaw-http-percent-alias-domain");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create percent-alias workspace");

        let config = test_config_with_streamable_http_server(format!("{origin}{canonical_suffix}"));
        let mut first = TemporaryMcpSession::new("http", &config, &workspace)
            .await
            .expect("canonical endpoint should initialize");
        first.shutdown().await;
        let initialize_count = server_state.next_generation.load(Ordering::Acquire);
        let unexpected_count = server_state
            .unexpected_request_count
            .load(Ordering::Acquire);

        let alias_config =
            test_config_with_streamable_http_server(format!("{origin}{alias_suffix}"));
        let error = match TemporaryMcpSession::new("http", &alias_config, &workspace).await {
            Ok(_) => panic!("an encoded unreserved alias must observe the existing quarantine"),
            Err(error) => error,
        };
        assert_eq!(error, HTTP_MCP_CLEANUP_UNCERTAIN_ERROR);
        assert_eq!(
            server_state.next_generation.load(Ordering::Acquire),
            initialize_count,
            "the alias must be rejected before initialize reaches the endpoint"
        );
        assert_eq!(
            server_state
                .unexpected_request_count
                .load(Ordering::Acquire),
            unexpected_count,
            "an encoded path alias must not reach a fallback route"
        );

        server_state.release_delete.notify_one();
        clear_mcp_caches_for_test().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streamable_http_transport_rejects_redirects_before_remote_side_effects() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-transport-redirect");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create redirect transport workspace");
    let (origin, server_state, server_task) = spawn_redirect_transport_http_test_server().await;
    cleanup.track_task(server_task);

    for alias in ["alias307", "alias308"] {
        let config = test_config_with_streamable_http_server(format!("{origin}/{alias}"));
        let result = TemporaryMcpSession::new("http", &config, &workspace).await;
        assert!(
            result.is_err(),
            "{alias} must not be followed by the transport"
        );
    }
    assert_eq!(server_state.alias_request_count.load(Ordering::Acquire), 2);
    assert_eq!(
        server_state.target_request_count.load(Ordering::Acquire),
        0,
        "307/308 aliases must never forward initialize to the MCP target"
    );

    let oauth_client = reqwest_client_with_timeout(2).expect("build OAuth/discovery client");
    let oauth_probe = oauth_client
        .post(format!("{origin}/alias307"))
        .json(&json!({"oauth": "discovery-probe"}))
        .send()
        .await
        .expect("OAuth/discovery client should retain its independent redirect policy");
    assert_eq!(oauth_probe.status(), StatusCode::OK);
    assert_eq!(
        server_state.target_request_count.load(Ordering::Acquire),
        1,
        "disabling redirects for MCP transport must not change OAuth/discovery clients"
    );

    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn endpoint_quarantine_blocks_cross_mode_initialization() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-cross-mode-quarantine");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create cross-mode quarantine workspace");
    let (url, server_state, server_task) =
        spawn_same_id_reuse_http_test_server_with_delete(StatusCode::ACCEPTED, true).await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .get("http")
        .expect("cross-mode HTTP server")
        .clone();
    let workspace_root =
        resolve_path_checked(".", &workspace).expect("resolve cross-mode workspace");

    let mut one_shot = TemporaryMcpSession::new("http", &config, &workspace)
        .await
        .expect("one-shot Session should initialize");
    one_shot.shutdown().await;
    let initialize_count = server_state.next_generation.load(Ordering::Acquire);
    let ordinary = initialize_http_session(
        "http",
        &server,
        "http\nordinary-after-one-shot",
        &workspace_root,
        &McpClientCapabilityPolicy::default(),
        false,
        2,
    )
    .await;
    assert!(
        ordinary.is_err(),
        "an ordinary cached Session must observe an ambiguous one-shot endpoint cleanup"
    );
    assert_eq!(
        server_state.next_generation.load(Ordering::Acquire),
        initialize_count,
        "ordinary initialization must be rejected before a request reaches the endpoint"
    );

    server_state.release_delete.notify_waiters();
    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_endpoint_quarantine_blocks_one_shot_initialization() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-ordinary-to-one-shot-quarantine");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create ordinary quarantine workspace");
    let (url, server_state, server_task) =
        spawn_same_id_reuse_http_test_server_with_delete(StatusCode::ACCEPTED, true).await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .get("http")
        .expect("ordinary HTTP server")
        .clone();
    let workspace_root = resolve_path_checked(".", &workspace).expect("resolve ordinary workspace");
    let cache_key = "http\nordinary-before-one-shot";

    initialize_http_session(
        "http",
        &server,
        cache_key,
        &workspace_root,
        &McpClientCapabilityPolicy::default(),
        false,
        2,
    )
    .await
    .expect("ordinary Session should initialize");
    assert_eq!(
        terminate_http_session("http", cache_key, &server).await,
        HttpDeleteOutcome::Ambiguous
    );
    let initialize_count = server_state.next_generation.load(Ordering::Acquire);
    let one_shot = TemporaryMcpSession::new("http", &config, &workspace).await;
    assert!(
        one_shot.is_err(),
        "one-shot initialization must observe an ambiguous ordinary endpoint cleanup"
    );
    assert_eq!(
        server_state.next_generation.load(Ordering::Acquire),
        initialize_count
    );

    server_state.release_delete.notify_waiters();
    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_cleanup_does_not_delete_a_same_id_replacement_in_another_cache_key() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-cross-key-same-id");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create cross-key same-ID workspace");
    let (url, server_state, server_task) =
        spawn_same_id_reuse_http_test_server_with_delete(StatusCode::NO_CONTENT, true).await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .get("http")
        .expect("same-ID HTTP server")
        .clone();
    let workspace_root = resolve_path_checked(".", &workspace).expect("resolve same-ID workspace");
    let capabilities = McpClientCapabilityPolicy::default();
    let first_key = "http\nordinary-same-id-a";
    let second_key = "http\nordinary-same-id-b";

    initialize_http_session(
        "http",
        &server,
        first_key,
        &workspace_root,
        &capabilities,
        false,
        2,
    )
    .await
    .expect("first ordinary Session should initialize");
    initialize_http_session(
        "http",
        &server,
        second_key,
        &workspace_root,
        &capabilities,
        false,
        2,
    )
    .await
    .expect("replacement ordinary Session should initialize");
    assert!(
        cached_http_session_identity_unchecked(first_key).is_none(),
        "installing the same remote Session ID under a new key must atomically retire the old key"
    );
    let requests_before_old_key_retry = server_state.normal_request_count.load(Ordering::Acquire);
    let old_key_error = http_post_json(
        "http",
        &server,
        first_key,
        &workspace_root,
        &capabilities,
        json!({
            "jsonrpc": "2.0",
            "id": 70,
            "method": "ping",
            "params": {}
        }),
        Some("fixed-session-id".to_string()),
        2,
    )
    .await
    .expect_err("the retired cache key must fail before sending another request");
    assert!(old_key_error.contains("superseded"), "{old_key_error}");
    assert_eq!(
        server_state.normal_request_count.load(Ordering::Acquire),
        requests_before_old_key_retry,
        "the retired cache key must not reach the endpoint"
    );
    assert_eq!(
        terminate_http_session("http", first_key, &server).await,
        HttpDeleteOutcome::Confirmed
    );
    assert_eq!(
        server_state.delete_count.load(Ordering::Acquire),
        0,
        "cleanup of the old cache key must not send DELETE for an ID owned by another key"
    );
    assert_eq!(
        checked_http_session_id("http", &server, second_key)
            .await
            .expect("read replacement identity")
            .as_deref(),
        Some("fixed-session-id")
    );

    terminate_http_session("http", second_key, &server).await;
    server_state.release_delete.notify_waiters();
    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_id_install_retires_old_requests_events_streams_and_descriptors() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-same-id-retirement");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create same-ID retirement workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_old_response.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("same-ID retirement server")
            .clone();
        let workspace_root =
            resolve_path_checked(".", &workspace).expect("resolve same-ID retirement workspace");
        let capabilities = McpClientCapabilityPolicy::default();
        let first_key = "http\nretired-same-id-a";
        let second_key = "http\nretired-same-id-b";

        initialize_http_session(
            "http",
            &server,
            first_key,
            &workspace_root,
            &capabilities,
            false,
            2,
        )
        .await
        .expect("initialize old same-ID Session");
        let first_identity = cached_http_session_identity_unchecked(first_key)
            .expect("old same-ID identity should be cached");
        set_http_last_event_id(first_key, &first_identity, "old-event");
        seed_empty_http_descriptor_caches(first_key);
        {
            let mut state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            state.stream_tasks.insert(
                first_key.to_string(),
                HttpStreamTaskEntry {
                    task_id: next_http_stream_task_id(),
                    epoch: first_identity.epoch,
                    generation: first_identity.generation,
                    handle: tokio::spawn(std::future::pending()),
                },
            );
        }

        let old_response = cleanup.spawn_worker({
            let server = server.clone();
            let workspace_root = workspace_root.clone();
            let capabilities = capabilities.clone();
            async move {
                http_post_json(
                    "http",
                    &server,
                    first_key,
                    &workspace_root,
                    &capabilities,
                    json!({
                        "jsonrpc": "2.0",
                        "id": 71,
                        "method": "tools/list",
                        "params": {}
                    }),
                    Some("fixed-session-id".to_string()),
                    5,
                )
                .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.old_response_started.notified(),
        )
        .await
        .expect("old request should reach its response barrier");

        initialize_http_session(
            "http",
            &server,
            second_key,
            &workspace_root,
            &capabilities,
            false,
            2,
        )
        .await
        .expect("initialize replacement same-ID Session");
        assert!(cached_http_session_identity_unchecked(first_key).is_none());
        {
            let state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            assert!(!state.last_event_ids.contains_key(first_key));
            assert!(!state.stream_tasks.contains_key(first_key));
        }
        assert_http_descriptor_caches_absent(first_key);

        let requests_before_retry = server_state.normal_request_count.load(Ordering::Acquire);
        let retry_error = http_post_json(
            "http",
            &server,
            first_key,
            &workspace_root,
            &capabilities,
            json!({"jsonrpc": "2.0", "id": 72, "method": "ping", "params": {}}),
            Some("fixed-session-id".to_string()),
            2,
        )
        .await
        .expect_err("a retired key must reject a fresh request before network I/O");
        assert_eq!(retry_error, HTTP_MCP_SESSION_SUPERSEDED_ERROR);
        assert_eq!(
            server_state.normal_request_count.load(Ordering::Acquire),
            requests_before_retry
        );

        server_state.release_old_response.notify_one();
        let stale_error = old_response
            .await
            .expect("join old same-ID response")
            .expect_err("the old in-flight response must be rejected");
        assert!(
            stale_error.contains("invalidated request epoch"),
            "{stale_error}"
        );
        assert_eq!(server_state.delete_count.load(Ordering::Acquire), 0);
        assert_eq!(
            cached_http_session_identity_unchecked(second_key)
                .expect("replacement identity must remain current")
                .session_id,
            "fixed-session-id"
        );

        remove_http_session(second_key);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_tools_list_cannot_cache_after_same_id_replacement() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-tools-cache-cas");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create tools cache-CAS workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_old_response.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("tools cache-CAS server");
        let cache_key =
            cache_key("http", server, &workspace, &config).expect("build tools cache-CAS key");
        let workspace_root =
            resolve_path_checked(".", &workspace).expect("resolve tools cache-CAS workspace");
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        install_http_descriptor_insert_barrier(&cache_key, "tools", reached_tx, release_rx);

        let list = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move { list_server_tools("http", &config, &workspace).await }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.old_response_started.notified(),
        )
        .await
        .expect("tools/list should reach the real server response barrier");
        server_state.release_old_response.notify_one();
        tokio::time::timeout(Duration::from_secs(2), reached_rx)
            .await
            .expect("tools/list should reach the post-response cache-insert barrier")
            .expect("tools cache-insert barrier sender should remain alive");

        let replacement_key = "http\ntools-cache-cas-replacement";
        initialize_http_session(
            "http",
            server,
            replacement_key,
            &workspace_root,
            &McpClientCapabilityPolicy::default(),
            false,
            2,
        )
        .await
        .expect("install same-ID replacement before the old tools cache write");
        assert!(cached_http_session_identity_unchecked(&cache_key).is_none());
        release_tx
            .send(())
            .expect("release tools cache-insert barrier");
        let error = list
            .await
            .expect("join late tools/list")
            .expect_err("the old tools descriptor write must lose its generation CAS");
        assert!(error.contains("superseded before caching"), "{error}");
        assert_http_descriptor_caches_absent(&cache_key);

        remove_http_session(replacement_key);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn descriptor_cache_cas_rejects_late_resource_prompt_and_catalog_writes() {
    run_panic_safe_mcp_test!(cleanup, {
        for cache_kind in ["resources", "prompts"] {
            let workspace = unique_temp_workspace(&format!("lingclaw-http-{cache_kind}-cache-cas"));
            cleanup.track_path(workspace.clone());
            fs::create_dir_all(&workspace).expect("create descriptor cache-CAS workspace");
            let (url, server_task) = spawn_resources_only_streamable_http_test_server().await;
            cleanup.track_task(server_task);
            let config = test_config_with_streamable_http_server(url);
            let server = config
                .mcp_servers
                .get("http")
                .expect("descriptor cache-CAS server");
            let cache_key = cache_key("http", server, &workspace, &config)
                .expect("build descriptor cache-CAS key");
            let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            install_http_descriptor_insert_barrier(&cache_key, cache_kind, reached_tx, release_rx);

            let list = cleanup.spawn_worker({
                let config = config.clone();
                let workspace = workspace.clone();
                async move {
                    match cache_kind {
                        "resources" => list_server_resources("http", &config, &workspace)
                            .await
                            .map(|items| items.len()),
                        "prompts" => list_server_prompts("http", &config, &workspace)
                            .await
                            .map(|items| items.len()),
                        _ => unreachable!("covered descriptor cache kind"),
                    }
                }
            });
            tokio::time::timeout(Duration::from_secs(2), reached_rx)
                .await
                .unwrap_or_else(|_| panic!("{cache_kind} should reach its cache-insert barrier"))
                .expect("descriptor cache-insert barrier sender should remain alive");

            let notification = match cache_kind {
                "resources" => "notifications/resources/list_changed",
                "prompts" => "notifications/prompts/list_changed",
                _ => unreachable!("covered descriptor cache kind"),
            };
            handle_http_server_message(
                &json!({"jsonrpc": "2.0", "method": notification, "params": {}}),
                &cache_key,
            );
            release_tx
                .send(())
                .expect("release descriptor cache-insert barrier");
            let error = list
                .await
                .expect("join late descriptor list")
                .expect_err("list-changed must invalidate an already-returned descriptor response");
            assert!(error.contains("superseded before caching"), "{error}");
            assert_http_descriptor_caches_absent(&cache_key);
            clear_mcp_caches_for_test().await;
        }

        let workspace = unique_temp_workspace("lingclaw-http-catalog-cache-cas");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create catalog cache-CAS workspace");
        let (url, server_task) = spawn_resources_only_streamable_http_test_server().await;
        cleanup.track_task(server_task);
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("catalog cache-CAS server");
        let cache_key =
            cache_key("http", server, &workspace, &config).expect("build catalog cache-CAS key");
        let (reached_tx, reached_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        install_http_descriptor_insert_barrier(&cache_key, "catalog", reached_tx, release_rx);

        let refresh = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move { refresh_servers(&config, &workspace).await }
        });
        tokio::time::timeout(Duration::from_secs(3), reached_rx)
            .await
            .expect("catalog load should reach its post-response cache-insert barrier")
            .expect("catalog cache-insert barrier sender should remain alive");
        invalidate_runtime_state_without_remote_shutdown().await;
        release_tx
            .send(())
            .expect("release catalog cache-insert barrier");
        let error = refresh
            .await
            .expect("join late catalog load")
            .expect_err("Settings invalidation must reject every late catalog cache write");
        assert!(error.contains("catalog was superseded"), "{error}");
        assert_http_descriptor_caches_absent(&cache_key);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_shot_same_id_install_retires_an_ordinary_cache_key() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-one-shot-retires-ordinary");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create one-shot retirement workspace");
    let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .get("http")
        .expect("one-shot retirement server")
        .clone();
    let workspace_root =
        resolve_path_checked(".", &workspace).expect("resolve one-shot retirement workspace");
    let ordinary_key = "http\nordinary-retired-by-one-shot";
    let capabilities = McpClientCapabilityPolicy::default();

    initialize_http_session(
        "http",
        &server,
        ordinary_key,
        &workspace_root,
        &capabilities,
        false,
        2,
    )
    .await
    .expect("initialize ordinary Session before one-shot");
    let one_shot = TemporaryMcpSession::new("http", &config, &workspace)
        .await
        .expect("one-shot replacement should initialize");
    assert!(cached_http_session_identity_unchecked(ordinary_key).is_none());
    let requests_before_retry = server_state.normal_request_count.load(Ordering::Acquire);
    let retry_error = http_post_json(
        "http",
        &server,
        ordinary_key,
        &workspace_root,
        &capabilities,
        json!({"jsonrpc": "2.0", "id": 73, "method": "ping", "params": {}}),
        Some("fixed-session-id".to_string()),
        2,
    )
    .await
    .expect_err("ordinary key retired by one-shot must fail before send");
    assert_eq!(retry_error, HTTP_MCP_SESSION_SUPERSEDED_ERROR);
    assert_eq!(
        server_state.normal_request_count.load(Ordering::Acquire),
        requests_before_retry
    );

    drop(one_shot);
    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn equal_session_ids_on_different_endpoints_do_not_retire_each_other() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-different-endpoint-same-id");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create different-endpoint workspace");
    let (first_url, _first_state, first_task) = spawn_same_id_reuse_http_test_server().await;
    let (second_url, _second_state, second_task) = spawn_same_id_reuse_http_test_server().await;
    cleanup.track_task(first_task);
    cleanup.track_task(second_task);
    let first_config = test_config_with_streamable_http_server(first_url);
    let second_config = test_config_with_streamable_http_server(second_url);
    let first_server = first_config
        .mcp_servers
        .get("http")
        .expect("first endpoint");
    let second_server = second_config
        .mcp_servers
        .get("http")
        .expect("second endpoint");
    let workspace_root =
        resolve_path_checked(".", &workspace).expect("resolve different-endpoint workspace");
    let capabilities = McpClientCapabilityPolicy::default();
    let first_key = "http\ndifferent-endpoint-a";
    let second_key = "http\ndifferent-endpoint-b";

    initialize_http_session(
        "http",
        first_server,
        first_key,
        &workspace_root,
        &capabilities,
        false,
        2,
    )
    .await
    .expect("initialize first endpoint");
    initialize_http_session(
        "http",
        second_server,
        second_key,
        &workspace_root,
        &capabilities,
        false,
        2,
    )
    .await
    .expect("initialize second endpoint");
    assert!(cached_http_session_identity_unchecked(first_key).is_some());
    assert!(cached_http_session_identity_unchecked(second_key).is_some());

    remove_http_session(first_key);
    remove_http_session(second_key);
    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_http_session_is_deleted_before_the_production_call_reinitializes() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-idle-controlled-cleanup");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create idle-cleanup workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_delete.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("idle-cleanup HTTP server");
        let cache_key =
            cache_key("http", server, &workspace, &config).expect("build idle-cleanup cache key");

        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm cached HTTP Session");
        seed_empty_http_descriptor_caches(&cache_key);
        {
            let mut state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            state
                .sessions
                .get_mut(&cache_key)
                .expect("cached HTTP Session")
                .last_used_at = Instant::now() - session_idle_ttl() - Duration::from_secs(1);
        }

        let retry = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move { call_http_server("http", &config, &workspace, "ping", json!({})).await }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("idle production call must send a controlled DELETE before reinitializing");
        assert_http_descriptor_caches_absent(&cache_key);
        assert_eq!(
            server_state.next_generation.load(Ordering::Acquire),
            3,
            "reinitialize must not be sent before the old Session DELETE completes"
        );
        server_state.release_delete.notify_one();
        retry
            .await
            .expect("join idle production retry")
            .expect("confirmed idle cleanup should permit reinitialize");
        assert_eq!(server_state.delete_count.load(Ordering::Acquire), 1);
        assert_eq!(server_state.next_generation.load(Ordering::Acquire), 4);
        assert_eq!(
            server_state.deleted_generations.lock().await.as_slice(),
            &[2],
            "the fixed remote ID must be deleted before generation 3 can reuse it"
        );

        server_state.release_delete.notify_waiters();
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ambiguous_idle_delete_quarantines_before_any_reinitialize() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-idle-accepted-cleanup");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create accepted idle-cleanup workspace");
    let (url, server_state, server_task) =
        spawn_same_id_reuse_http_test_server_with_delete(StatusCode::ACCEPTED, true).await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .get("http")
        .expect("accepted idle-cleanup server");
    let cache_key = cache_key("http", server, &workspace, &config)
        .expect("build accepted idle-cleanup cache key");
    let domain_key = http_remote_cleanup_domain_key(server).expect("idle cleanup domain");

    call_http_server("http", &config, &workspace, "ping", json!({}))
        .await
        .expect("warm accepted idle Session");
    {
        let mut state = http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock");
        state
            .sessions
            .get_mut(&cache_key)
            .expect("accepted idle Session")
            .last_used_at = Instant::now() - session_idle_ttl() - Duration::from_secs(1);
    }
    let initialize_count = server_state.next_generation.load(Ordering::Acquire);
    let error = call_http_server("http", &config, &workspace, "ping", json!({}))
        .await
        .expect_err("202 idle DELETE must not permit reinitialize");
    assert_eq!(error, HTTP_MCP_CLEANUP_UNCERTAIN_ERROR);
    assert_eq!(
        http_cleanup_phase(&domain_key),
        Some(HttpCleanupPhase::Uncertain)
    );
    assert_eq!(
        server_state.next_generation.load(Ordering::Acquire),
        initialize_count,
        "ambiguous idle cleanup must stop before initialize"
    );
    let workspace_root =
        resolve_path_checked(".", &workspace).expect("resolve accepted idle workspace");
    let replacement = initialize_http_session(
        "http",
        server,
        "http\naccepted-idle-replacement",
        &workspace_root,
        &McpClientCapabilityPolicy::default(),
        false,
        2,
    )
    .await;
    assert!(replacement.is_err());
    assert_eq!(
        server_state.next_generation.load(Ordering::Acquire),
        initialize_count,
        "same-endpoint replacement must be rejected before send"
    );

    let (independent_url, _independent_state, independent_task) =
        spawn_same_id_reuse_http_test_server().await;
    cleanup.track_task(independent_task);
    let independent_config = test_config_with_streamable_http_server(independent_url);
    let independent_server = independent_config
        .mcp_servers
        .get("http")
        .expect("independent idle endpoint");
    initialize_http_session(
        "http",
        independent_server,
        "http\nindependent-idle-endpoint",
        &workspace_root,
        &McpClientCapabilityPolicy::default(),
        false,
        2,
    )
    .await
    .expect("an unrelated endpoint must remain available");

    server_state.release_delete.notify_one();
    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timed_out_idle_delete_blocks_reinitialize_and_keeps_state_bounded() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-idle-timeout-cleanup");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create timed-out idle-cleanup workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_delete.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("timed-out idle-cleanup server");
        let cache_key = cache_key("http", server, &workspace, &config)
            .expect("build timed-out idle-cleanup cache key");
        let domain_key = http_remote_cleanup_domain_key(server).expect("timed-out idle domain");

        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm timed-out idle Session");
        {
            let mut state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            state
                .sessions
                .get_mut(&cache_key)
                .expect("timed-out idle Session")
                .last_used_at = Instant::now() - session_idle_ttl() - Duration::from_secs(1);
        }
        let initialize_count = server_state.next_generation.load(Ordering::Acquire);
        let retry = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move { call_http_server("http", &config, &workspace, "ping", json!({})).await }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("idle DELETE should reach the timeout barrier");
        let error = tokio::time::timeout(Duration::from_secs(4), retry)
            .await
            .expect("idle DELETE client timeout must be bounded")
            .expect("join timed-out idle call")
            .expect_err("timed-out idle DELETE must fail closed");
        assert_eq!(error, HTTP_MCP_CLEANUP_UNCERTAIN_ERROR);
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain)
        );
        assert_eq!(
            server_state.next_generation.load(Ordering::Acquire),
            initialize_count
        );
        {
            let state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            assert!(!state.sessions.contains_key(&cache_key));
            assert!(state.supersessions.is_empty());
            assert!(!state.cleanup_tasks.contains_key(&domain_key));
            assert_eq!(
                state
                    .cleanups
                    .keys()
                    .filter(|key| key.as_str() == domain_key)
                    .count(),
                1,
                "one endpoint timeout must retain one bounded domain tombstone"
            );
        }

        let delete_finished = server_state.delete_finished.notified();
        server_state.release_delete.notify_one();
        tokio::time::timeout(Duration::from_secs(2), delete_finished)
            .await
            .expect("timed-out server DELETE should eventually finish");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_idle_cleanup_caller_keeps_endpoint_quarantined() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-idle-abort-cleanup");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create aborted idle-cleanup workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_delete.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("aborted idle-cleanup server");
        let cache_key = cache_key("http", server, &workspace, &config)
            .expect("build aborted idle-cleanup cache key");
        let domain_key = http_remote_cleanup_domain_key(server).expect("aborted idle domain");

        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm aborted idle Session");
        {
            let mut state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            state
                .sessions
                .get_mut(&cache_key)
                .expect("aborted idle Session")
                .last_used_at = Instant::now() - session_idle_ttl() - Duration::from_secs(1);
        }
        let initialize_count = server_state.next_generation.load(Ordering::Acquire);
        let retry = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move { call_http_server("http", &config, &workspace, "ping", json!({})).await }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("idle DELETE should reach the abort barrier");
        retry.abort();
        assert!(
            retry
                .await
                .expect_err("idle cleanup caller should be cancelled")
                .is_cancelled()
        );
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Pending),
            "the runtime-owned DELETE must keep its endpoint quarantine after the caller is cancelled"
        );
        let workspace_root =
            resolve_path_checked(".", &workspace).expect("resolve aborted idle workspace");
        let replacement = initialize_http_session(
            "http",
            server,
            "http\naborted-idle-replacement",
            &workspace_root,
            &McpClientCapabilityPolicy::default(),
            false,
            2,
        )
        .await;
        assert!(replacement.is_err());
        assert_eq!(
            server_state.next_generation.load(Ordering::Acquire),
            initialize_count,
            "caller cancellation must block replacement before send"
        );

        let delete_finished = server_state.delete_finished.notified();
        server_state.release_delete.notify_one();
        tokio::time::timeout(Duration::from_secs(2), delete_finished)
            .await
            .expect("runtime-owned idle DELETE should finish after caller abort");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_idle_callers_share_one_cleanup_and_one_reinitialize() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-idle-concurrent-cleanup");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create concurrent idle-cleanup workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_delete.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("concurrent idle-cleanup server");
        let cache_key = cache_key("http", server, &workspace, &config)
            .expect("build concurrent idle-cleanup cache key");

        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm concurrent idle Session");
        {
            let mut state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            state
                .sessions
                .get_mut(&cache_key)
                .expect("concurrent idle Session")
                .last_used_at = Instant::now() - session_idle_ttl() - Duration::from_secs(1);
        }
        let initialize_count = server_state.next_generation.load(Ordering::Acquire);
        let left = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move { call_http_server("http", &config, &workspace, "ping", json!({})).await }
        });
        let right = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move { call_http_server("http", &config, &workspace, "ping", json!({})).await }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("one concurrent caller should start idle DELETE");
        tokio::task::yield_now().await;
        assert_eq!(server_state.delete_count.load(Ordering::Acquire), 1);
        assert_eq!(
            server_state.next_generation.load(Ordering::Acquire),
            initialize_count
        );

        server_state.release_delete.notify_one();
        left.await
            .expect("join left idle caller")
            .expect("left idle caller should converge");
        right
            .await
            .expect("join right idle caller")
            .expect("right idle caller should converge");
        assert_eq!(server_state.delete_count.load(Ordering::Acquire), 1);
        assert_eq!(
            server_state.next_generation.load(Ordering::Acquire),
            initialize_count + 1,
            "concurrent callers must share one replacement initialize"
        );
        assert!(http_cleanup_phase(&cache_key).is_none());

        remove_http_session(&cache_key);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn idle_cleanup_waits_for_an_inflight_ordinary_request_generation() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-idle-inflight-request");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create in-flight idle workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || {
                server_state.release_old_response.notify_waiters();
                server_state.release_delete.notify_waiters();
            }
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("in-flight idle server");
        let cache_key =
            cache_key("http", server, &workspace, &config).expect("build in-flight idle cache key");

        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm in-flight idle Session");
        let initialize_count = server_state.next_generation.load(Ordering::Acquire);
        let blocked =
            cleanup.spawn_worker({
                let config = config.clone();
                let workspace = workspace.clone();
                async move {
                    call_http_server("http", &config, &workspace, "tools/list", json!({})).await
                }
            });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.old_response_started.notified(),
        )
        .await
        .expect("ordinary tools/list must reach the server barrier");
        {
            let mut state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            assert_eq!(
                state
                    .in_flight_requests
                    .iter()
                    .filter(|(key, _)| key.cache_key == cache_key)
                    .map(|(_, count)| *count)
                    .sum::<usize>(),
                1
            );
            state
                .sessions
                .get_mut(&cache_key)
                .expect("cached in-flight idle Session")
                .last_used_at = Instant::now() - session_idle_ttl() - Duration::from_secs(1);
        }

        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("a concurrent caller must reuse rather than delete the busy generation");
        assert_eq!(server_state.delete_count.load(Ordering::Acquire), 0);
        assert_eq!(
            server_state.next_generation.load(Ordering::Acquire),
            initialize_count,
            "no replacement initialize may race the in-flight ordinary request"
        );

        server_state.release_old_response.notify_one();
        blocked
            .await
            .expect("join blocked ordinary request")
            .expect("blocked ordinary request should finish on its original generation");
        assert!(
            http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock")
                .in_flight_requests
                .keys()
                .all(|key| key.cache_key != cache_key)
        );

        {
            let mut state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            state
                .sessions
                .get_mut(&cache_key)
                .expect("cached Session after in-flight completion")
                .last_used_at = Instant::now() - session_idle_ttl() - Duration::from_secs(1);
        }
        let retry = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move { call_http_server("http", &config, &workspace, "ping", json!({})).await }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("idle cleanup should start once the ordinary request lease is gone");
        assert_eq!(server_state.delete_count.load(Ordering::Acquire), 1);
        assert_eq!(
            server_state.next_generation.load(Ordering::Acquire),
            initialize_count
        );
        server_state.release_delete.notify_one();
        retry
            .await
            .expect("join post-in-flight idle retry")
            .expect("confirmed cleanup should allow exactly one replacement");
        assert_eq!(
            server_state.next_generation.load(Ordering::Acquire),
            initialize_count + 1
        );
    });
}

async fn wait_for_registered_deferred_cleanup(cache_key: &str) -> HttpInFlightRequestKey {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let key = {
                let state = http_runtime_state()
                    .lock()
                    .expect("HTTP runtime state lock");
                state.cleanup_tasks.contains_key(cache_key).then(|| {
                    state
                        .deferred_cleanups
                        .keys()
                        .find(|key| key.cache_key == cache_key)
                        .cloned()
                })
            }
            .flatten();
            if let Some(key) = key {
                return key;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("deferred cleanup should be registered")
}

async fn wait_for_deferred_cleanup_task_exit(cache_key: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let exited = {
                let state = http_runtime_state()
                    .lock()
                    .expect("HTTP runtime state lock");
                !state.cleanup_tasks.contains_key(cache_key)
                    && state
                        .deferred_cleanups
                        .keys()
                        .all(|key| key.cache_key != cache_key)
            };
            if exited {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("deferred cleanup task should exit");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deferred_cleanup_not_found_waits_for_every_request_before_delete_and_reinitialize() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-deferred-not-found");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create deferred 404 workspace");
        let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let state = state.clone();
            move || {
                state.release_first_not_found.notify_waiters();
                state.release_second_request.notify_waiters();
                state.release_delete.notify_waiters();
                state.release_initialize.notify_waiters();
            }
        });
        let mut config = test_config_with_streamable_http_server(url);
        config
            .mcp_servers
            .get_mut("http")
            .expect("deferred 404 server config")
            .timeout_secs = Some(20);
        let server = config.mcp_servers.get("http").expect("deferred 404 server");
        let cache_key =
            cache_key("http", server, &workspace, &config).expect("deferred 404 cache key");
        let domain_key = http_remote_cleanup_domain_key(server).expect("deferred 404 domain");

        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm deferred 404 Session");
        let initial_initialize_count = state.initialize_count.load(Ordering::Acquire);
        let first = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server(
                    "http",
                    &config,
                    &workspace,
                    "deferred/first-not-found",
                    json!({}),
                )
                .await
            }
        });
        state.first_not_found_started.notified().await;
        let second = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server("http", &config, &workspace, "deferred/second", json!({})).await
            }
        });
        state.second_request_started.notified().await;

        state.release_first_not_found.notify_one();
        let deferred_key = wait_for_registered_deferred_cleanup(&cache_key).await;
        assert_eq!(
            http_cleanup_phase(&cache_key),
            Some(HttpCleanupPhase::Pending)
        );
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Pending)
        );
        assert_eq!(state.delete_count.load(Ordering::Acquire), 0);
        assert!(cached_http_session_identity_unchecked(&cache_key).is_none());
        let ordinary_methods = state.ordinary_methods.lock().await.clone();
        let blocked = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move { call_http_server("http", &config, &workspace, "ping", json!({})).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            state.ordinary_methods.lock().await.as_slice(),
            ordinary_methods
        );
        assert_eq!(
            state.initialize_count.load(Ordering::Acquire),
            initial_initialize_count,
            "no replacement initialize may precede the deferred DELETE"
        );

        let other_workspace = unique_temp_workspace("lingclaw-http-deferred-other-endpoint");
        cleanup.track_path(other_workspace.clone());
        fs::create_dir_all(&other_workspace).expect("create independent endpoint workspace");
        let (other_url, other_state, other_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(other_task);
        let other_config = test_config_with_streamable_http_server(other_url);
        call_http_server("http", &other_config, &other_workspace, "ping", json!({}))
            .await
            .expect("independent endpoint must remain available");
        assert_eq!(other_state.initialize_count.load(Ordering::Acquire), 1);

        let delete_started = state.delete_started.notified();
        state.release_second_request.notify_one();
        tokio::time::timeout(Duration::from_secs(2), delete_started)
            .await
            .expect("last lease should start exactly one deferred DELETE");
        assert_eq!(state.delete_count.load(Ordering::Acquire), 1);
        assert!(
            !http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock")
                .in_flight_requests
                .contains_key(&deferred_key)
        );
        assert_eq!(
            state.initialize_count.load(Ordering::Acquire),
            initial_initialize_count
        );
        state.release_delete.notify_one();

        second
            .await
            .expect("join second deferred request")
            .expect_err("the old generation response must be rejected");
        first
            .await
            .expect("join first deferred request")
            .expect("confirmed cleanup should permit the production retry");
        blocked
            .await
            .expect("join caller waiting behind deferred cleanup")
            .expect("waiting caller should share the recovered generation");
        assert_eq!(state.delete_count.load(Ordering::Acquire), 1);
        assert_eq!(
            state.initialize_count.load(Ordering::Acquire),
            initial_initialize_count + 1
        );
        wait_for_deferred_cleanup_task_exit(&cache_key).await;
        {
            let runtime = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            assert!(!runtime.cleanups.contains_key(&cache_key));
            assert!(!runtime.cleanups.contains_key(&domain_key));
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deferred_cleanup_cancelled_last_request_stays_uncertain_after_one_delete() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-deferred-cancel");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create deferred cancel workspace");
        let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let state = state.clone();
            move || {
                state.release_first_not_found.notify_waiters();
                state.release_second_request.notify_waiters();
                state.release_delete.notify_waiters();
            }
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("deferred cancel server");
        let cache_key =
            cache_key("http", server, &workspace, &config).expect("deferred cancel cache key");
        let domain_key = http_remote_cleanup_domain_key(server).expect("deferred cancel domain");
        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm deferred cancel Session");

        let first = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server(
                    "http",
                    &config,
                    &workspace,
                    "deferred/first-not-found",
                    json!({}),
                )
                .await
            }
        });
        state.first_not_found_started.notified().await;
        let second = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server("http", &config, &workspace, "deferred/second", json!({})).await
            }
        });
        state.second_request_started.notified().await;
        state.release_first_not_found.notify_one();
        wait_for_registered_deferred_cleanup(&cache_key).await;

        let delete_started = state.delete_started.notified();
        second.abort();
        assert!(
            second
                .await
                .expect_err("second request should cancel")
                .is_cancelled()
        );
        tokio::time::timeout(Duration::from_secs(2), delete_started)
            .await
            .expect("cancelled last lease should wake the deferred cleanup once");
        state.release_delete.notify_one();
        first
            .await
            .expect("join first cancelled-generation request")
            .expect_err("an ambiguous cancelled request must block automatic recovery");
        wait_for_deferred_cleanup_task_exit(&cache_key).await;
        assert_eq!(state.delete_count.load(Ordering::Acquire), 1);
        assert_eq!(
            http_cleanup_phase(&cache_key),
            Some(HttpCleanupPhase::Uncertain)
        );
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain)
        );
        let initialize_count = state.initialize_count.load(Ordering::Acquire);
        assert_eq!(
            call_http_server("http", &config, &workspace, "ping", json!({}))
                .await
                .expect_err("uncertain deferred cleanup must stay isolated"),
            HTTP_MCP_CLEANUP_UNCERTAIN_ERROR
        );
        assert_eq!(
            state.initialize_count.load(Ordering::Acquire),
            initialize_count
        );
        state.release_second_request.notify_waiters();
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deferred_cleanup_task_abort_keeps_bounded_uncertain_state() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-deferred-task-abort");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create deferred task-abort workspace");
        let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let state = state.clone();
            move || {
                state.release_first_not_found.notify_waiters();
                state.release_second_request.notify_waiters();
                state.release_delete.notify_waiters();
            }
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("deferred abort server");
        let cache_key =
            cache_key("http", server, &workspace, &config).expect("deferred abort cache key");
        let domain_key = http_remote_cleanup_domain_key(server).expect("deferred abort domain");
        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm deferred task-abort Session");

        let first = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server(
                    "http",
                    &config,
                    &workspace,
                    "deferred/first-not-found",
                    json!({}),
                )
                .await
            }
        });
        state.first_not_found_started.notified().await;
        let second = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server("http", &config, &workspace, "deferred/second", json!({})).await
            }
        });
        state.second_request_started.notified().await;
        state.release_first_not_found.notify_one();
        wait_for_registered_deferred_cleanup(&cache_key).await;
        let cleanup_task_aborted = {
            let runtime = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            runtime
                .cleanup_tasks
                .get(&cache_key)
                .map(|entry| entry._handle.abort())
                .is_some()
        };
        assert!(cleanup_task_aborted, "runtime-owned deferred cleanup task");
        wait_for_deferred_cleanup_task_exit(&cache_key).await;
        assert_eq!(state.delete_count.load(Ordering::Acquire), 0);
        assert_eq!(
            http_cleanup_phase(&cache_key),
            Some(HttpCleanupPhase::Uncertain)
        );
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain)
        );

        state.release_second_request.notify_one();
        second
            .await
            .expect("join response after deferred task abort")
            .expect_err("old response must remain invalidated");
        first
            .await
            .expect("join caller after deferred task abort")
            .expect_err("aborted cleanup task must fail closed");
        assert_eq!(state.delete_count.load(Ordering::Acquire), 0);
        {
            let runtime = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            assert!(runtime.deferred_cleanups.is_empty());
            assert!(runtime.cleanup_tasks.is_empty());
            assert_eq!(
                runtime
                    .cleanups
                    .keys()
                    .filter(|key| *key == &cache_key || *key == &domain_key)
                    .count(),
                2,
                "one cache and one endpoint tombstone must remain bounded"
            );
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deferred_cleanup_old_generation_does_not_delete_a_same_id_replacement() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-deferred-aba-old");
        let replacement_workspace = unique_temp_workspace("lingclaw-http-deferred-aba-new");
        cleanup.track_path(workspace.clone());
        cleanup.track_path(replacement_workspace.clone());
        fs::create_dir_all(&workspace).expect("create deferred ABA old workspace");
        fs::create_dir_all(&replacement_workspace)
            .expect("create deferred ABA replacement workspace");
        let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let state = state.clone();
            move || {
                state.release_first_not_found.notify_waiters();
                state.release_second_request.notify_waiters();
                state.release_delete.notify_waiters();
            }
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config.mcp_servers.get("http").expect("deferred ABA server");
        let old_cache_key =
            cache_key("http", server, &workspace, &config).expect("old ABA cache key");
        let replacement_key = cache_key("http", server, &replacement_workspace, &config)
            .expect("replacement ABA cache key");
        let domain_key = http_remote_cleanup_domain_key(server).expect("deferred ABA domain");
        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm deferred ABA Session");

        let first = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server(
                    "http",
                    &config,
                    &workspace,
                    "deferred/first-not-found",
                    json!({}),
                )
                .await
            }
        });
        state.first_not_found_started.notified().await;
        let second = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server("http", &config, &workspace, "deferred/second", json!({})).await
            }
        });
        state.second_request_started.notified().await;
        state.release_first_not_found.notify_one();
        wait_for_registered_deferred_cleanup(&old_cache_key).await;
        first.abort();
        assert!(
            first
                .await
                .expect_err("old caller should cancel")
                .is_cancelled()
        );

        let replacement_root = resolve_path_checked(".", &replacement_workspace)
            .expect("resolve replacement ABA workspace");
        let replacement_identity = set_http_session_id(
            &replacement_key,
            Some("deferred-fixed-session".to_string()),
            &replacement_root,
            true,
        )
        .expect("install replacement ABA generation");
        http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock")
            .remote_domain_by_cache_key
            .insert(replacement_key.clone(), domain_key.clone());

        state.release_second_request.notify_one();
        second
            .await
            .expect("join old ABA response")
            .expect_err("old ABA response must remain invalidated");
        wait_for_deferred_cleanup_task_exit(&old_cache_key).await;
        assert_eq!(
            state.delete_count.load(Ordering::Acquire),
            0,
            "the old cleanup must not DELETE a same-ID replacement"
        );
        assert!(http_session_identity_is_current(
            &replacement_key,
            &replacement_identity
        ));

        remove_http_session(&replacement_key);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deferred_cleanup_body_timeout_stays_quarantined_without_reinitialize() {
    let guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let body_result = std::panic::AssertUnwindSafe(async {
        let workspace = unique_temp_workspace("lingclaw-http-deferred-body-timeout");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create deferred body-timeout workspace");
        let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let state = state.clone();
            move || state.release_second_request.notify_waiters()
        });
        let mut config = test_config_with_streamable_http_server(url);
        config
            .mcp_servers
            .get_mut("http")
            .expect("body-timeout server config")
            .timeout_secs = Some(1);
        let server = config.mcp_servers.get("http").expect("body-timeout server");
        let cache_key =
            cache_key("http", server, &workspace, &config).expect("body-timeout cache key");
        let domain_key = http_remote_cleanup_domain_key(server).expect("body-timeout domain");
        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm body-timeout Session");
        let initialize_count = state.initialize_count.load(Ordering::Acquire);

        let timed_out = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server(
                    "http",
                    &config,
                    &workspace,
                    "deferred/body-timeout",
                    json!({}),
                )
                .await
            }
        });
        state.body_timeout_started.notified().await;
        let second = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server("http", &config, &workspace, "deferred/second", json!({})).await
            }
        });
        state.second_request_started.notified().await;
        let error = tokio::time::timeout(Duration::from_secs(3), timed_out)
            .await
            .expect("body timeout should be bounded")
            .expect("join body-timeout request")
            .expect_err("pending response body must time out");
        assert!(error.contains("timed out"), "{error}");
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain)
        );
        assert!(cached_http_session_identity_unchecked(&cache_key).is_none());
        assert_eq!(state.delete_count.load(Ordering::Acquire), 0);
        assert_eq!(
            state.initialize_count.load(Ordering::Acquire),
            initialize_count
        );
        assert_eq!(
            call_http_server("http", &config, &workspace, "ping", json!({}))
                .await
                .expect_err("ambiguous body timeout must block reinitialize"),
            HTTP_MCP_CLEANUP_UNCERTAIN_ERROR
        );

        state.release_second_request.notify_one();
        second
            .await
            .expect("join concurrent body-timeout request")
            .expect_err("concurrent old response must not revive the Session");
        assert_eq!(state.delete_count.load(Ordering::Acquire), 0);
        assert_eq!(
            state.initialize_count.load(Ordering::Acquire),
            initialize_count
        );
        {
            let runtime = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            assert!(runtime.in_flight_requests.is_empty());
            assert!(runtime.deferred_cleanups.is_empty());
            assert!(runtime.cleanup_tasks.is_empty());
        }
    })
    .catch_unwind()
    .await;
    finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ordinary_timeouts_quarantine_before_waiting_for_cancel_notification() {
    let guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let body_result = std::panic::AssertUnwindSafe(async {
        for kind in [
            DeferredTimeoutKind::Send,
            DeferredTimeoutKind::Body,
            DeferredTimeoutKind::Sse,
        ] {
            let workspace = unique_temp_workspace(&format!(
                "lingclaw-http-timeout-before-cancel-{}",
                kind.label()
            ));
            cleanup.track_path(workspace.clone());
            fs::create_dir_all(&workspace).expect("create timeout-before-cancel workspace");
            let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
            cleanup.track_task(server_task);
            state.block_cancellation.store(true, Ordering::Release);
            cleanup.track_barrier_release({
                let state = state.clone();
                move || {
                    state.release_cancellation.notify_waiters();
                    kind.release_origin(&state);
                }
            });

            let mut config = test_config_with_streamable_http_server(url);
            config
                .mcp_servers
                .get_mut("http")
                .expect("timeout-before-cancel server config")
                .timeout_secs = Some(1);
            let server = config
                .mcp_servers
                .get("http")
                .expect("timeout-before-cancel server");
            let cache_key = cache_key("http", server, &workspace, &config)
                .expect("timeout-before-cancel cache key");
            let domain_key =
                http_remote_cleanup_domain_key(server).expect("timeout-before-cancel domain");

            call_http_server("http", &config, &workspace, "ping", json!({}))
                .await
                .expect("warm timeout-before-cancel Session");
            let initialize_count = state.initialize_count.load(Ordering::Acquire);
            let ordinary_count = state.ordinary_request_count.load(Ordering::Acquire);
            let request = cleanup.spawn_worker({
                let config = config.clone();
                let workspace = workspace.clone();
                async move {
                    call_http_server("http", &config, &workspace, kind.method(), json!({})).await
                }
            });
            tokio::time::timeout(Duration::from_secs(2), kind.started(&state).notified())
                .await
                .unwrap_or_else(|_| {
                    panic!("{} timeout request must reach the server", kind.label())
                });
            tokio::time::timeout(
                Duration::from_secs(4),
                state.cancellation_started.notified(),
            )
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{} timeout must attempt its cancellation notification",
                    kind.label()
                )
            });

            assert_eq!(
                http_cleanup_phase(&domain_key),
                Some(HttpCleanupPhase::Uncertain),
                "{} timeout must quarantine before cancellation returns",
                kind.label()
            );
            assert!(
                cached_http_session_identity_unchecked(&cache_key).is_none(),
                "{} timeout must detach its exact generation before cancellation returns",
                kind.label()
            );
            assert!(
                http_runtime_state()
                    .lock()
                    .expect("HTTP runtime state lock")
                    .in_flight_requests
                    .is_empty(),
                "{} timeout must release its exact request lease before cancellation returns",
                kind.label()
            );
            assert_eq!(
                call_http_server("http", &config, &workspace, "ping", json!({}))
                    .await
                    .expect_err("quarantined endpoint must reject a concurrent production call"),
                HTTP_MCP_CLEANUP_UNCERTAIN_ERROR
            );
            assert_eq!(
                state.initialize_count.load(Ordering::Acquire),
                initialize_count,
                "{} timeout must block reinitialize while cancellation is pending",
                kind.label()
            );
            assert_eq!(
                state.ordinary_request_count.load(Ordering::Acquire),
                ordinary_count + 1,
                "{} timeout must reject the second ordinary POST before network send",
                kind.label()
            );

            if matches!(kind, DeferredTimeoutKind::Send) {
                let other_workspace = unique_temp_workspace("lingclaw-http-timeout-other-endpoint");
                cleanup.track_path(other_workspace.clone());
                fs::create_dir_all(&other_workspace).expect("create unrelated endpoint workspace");
                let (other_url, _, other_server_task) =
                    spawn_deferred_cleanup_http_test_server().await;
                cleanup.track_task(other_server_task);
                let other_config = test_config_with_streamable_http_server(other_url);
                call_http_server("http", &other_config, &other_workspace, "ping", json!({}))
                    .await
                    .expect("an unrelated endpoint must remain usable");
            }

            state.release_cancellation.notify_waiters();
            kind.release_origin(&state);
            let error = tokio::time::timeout(Duration::from_secs(3), request)
                .await
                .expect("timeout request must finish after cancellation release")
                .expect("join timeout request")
                .expect_err("timeout request must fail");
            assert!(error.contains("timed out"), "{error}");
            assert_eq!(state.cancellation_count.load(Ordering::Acquire), 1);
            assert_eq!(
                http_cleanup_phase(&domain_key),
                Some(HttpCleanupPhase::Uncertain),
                "a successful cancellation notification must not downgrade quarantine"
            );

            clear_mcp_caches_for_test().await;
        }
    })
    .catch_unwind()
    .await;
    finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn later_timeout_makes_an_existing_endpoint_cleanup_sticky() {
    let guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let body_result = std::panic::AssertUnwindSafe(async {
        for status in [
            StatusCode::NO_CONTENT,
            StatusCode::NOT_FOUND,
            StatusCode::METHOD_NOT_ALLOWED,
        ] {
            let workspace = unique_temp_workspace(&format!(
                "lingclaw-http-pending-cleanup-timeout-{}",
                status.as_u16()
            ));
            cleanup.track_path(workspace.clone());
            fs::create_dir_all(&workspace).expect("create pending-cleanup timeout workspace");
            let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
            cleanup.track_task(server_task);
            state
                .delete_status
                .store(usize::from(status.as_u16()), Ordering::Release);
            state.block_cancellation.store(true, Ordering::Release);
            cleanup.track_barrier_release({
                let state = state.clone();
                move || {
                    state.release_delete.notify_waiters();
                    state.release_cancellation.notify_waiters();
                    state.release_send_timeout.notify_waiters();
                }
            });

            let mut config = test_config_with_streamable_http_server(url);
            config
                .mcp_servers
                .get_mut("http")
                .expect("pending-cleanup timeout server config")
                .timeout_secs = Some(1);
            let server = config
                .mcp_servers
                .get("http")
                .expect("pending-cleanup timeout server");
            let active_key = cache_key("http", server, &workspace, &config)
                .expect("build active timeout cache key");
            let domain_key =
                http_remote_cleanup_domain_key(server).expect("pending-cleanup timeout domain");
            call_http_server("http", &config, &workspace, "ping", json!({}))
                .await
                .expect("warm active timeout Session");

            let cleanup_key = format!("http\npending-cleanup-{}", status.as_u16());
            let workspace_root =
                resolve_path_checked(".", &workspace).expect("resolve pending-cleanup workspace");
            set_http_session_id(
                &cleanup_key,
                Some(format!("pending-delete-{}", status.as_u16())),
                &workspace_root,
                true,
            )
            .expect("install Session whose DELETE will remain pending");
            http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock")
                .remote_domain_by_cache_key
                .insert(cleanup_key.clone(), domain_key.clone());

            let timed_out = cleanup.spawn_worker({
                let config = config.clone();
                let workspace = workspace.clone();
                async move {
                    call_http_server(
                        "http",
                        &config,
                        &workspace,
                        DeferredTimeoutKind::Send.method(),
                        json!({}),
                    )
                    .await
                }
            });
            tokio::time::timeout(
                Duration::from_secs(2),
                state.send_timeout_started.notified(),
            )
            .await
            .expect("the sibling request must reach its timeout barrier");

            let pending_cleanup = cleanup.spawn_worker({
                let server = server.clone();
                let cleanup_key = cleanup_key.clone();
                async move { terminate_http_session("http", &cleanup_key, &server).await }
            });
            tokio::time::timeout(Duration::from_secs(2), state.delete_started.notified())
                .await
                .expect("the older endpoint DELETE must become pending");
            assert_eq!(
                http_cleanup_phase(&domain_key),
                Some(HttpCleanupPhase::Pending)
            );

            tokio::time::timeout(
                Duration::from_secs(4),
                state.cancellation_started.notified(),
            )
            .await
            .expect("the sibling timeout must reach its cancellation barrier");
            assert_eq!(
                http_cleanup_phase(&domain_key),
                Some(HttpCleanupPhase::Uncertain),
                "a later POST timeout must upgrade the older Pending endpoint cleanup"
            );
            assert!(cached_http_session_identity_unchecked(&active_key).is_none());

            state.release_delete.notify_one();
            let outcome = pending_cleanup.await.expect("join the older DELETE owner");
            let expected = if status == StatusCode::NO_CONTENT {
                HttpDeleteOutcome::Confirmed
            } else {
                HttpDeleteOutcome::NotApplied
            };
            assert_eq!(outcome, expected);
            assert_eq!(
                http_cleanup_phase(&domain_key),
                Some(HttpCleanupPhase::Uncertain),
                "an older {status} DELETE result must not clear newer timeout uncertainty"
            );

            let initialize_count = state.initialize_count.load(Ordering::Acquire);
            let ordinary_count = state.ordinary_request_count.load(Ordering::Acquire);
            assert_eq!(
                call_http_server("http", &config, &workspace, "ping", json!({}))
                    .await
                    .expect_err("sticky timeout uncertainty must block replacement before send"),
                HTTP_MCP_CLEANUP_UNCERTAIN_ERROR
            );
            assert_eq!(
                state.initialize_count.load(Ordering::Acquire),
                initialize_count
            );
            assert_eq!(
                state.ordinary_request_count.load(Ordering::Acquire),
                ordinary_count
            );

            state.release_cancellation.notify_waiters();
            state.release_send_timeout.notify_waiters();
            let error = tokio::time::timeout(Duration::from_secs(4), timed_out)
                .await
                .expect("timeout request must finish after cancellation release")
                .expect("join sibling timeout request")
                .expect_err("sibling request must remain timed out");
            assert!(error.contains("timed out"), "{error}");
            {
                let runtime = http_runtime_state()
                    .lock()
                    .expect("HTTP runtime state lock");
                assert_eq!(runtime.cleanups.len(), 1);
                assert!(runtime.cleanup_tasks.is_empty());
                assert!(runtime.deferred_cleanups.is_empty());
                assert!(runtime.in_flight_requests.is_empty());
            }
            clear_mcp_caches_for_test().await;
        }
    })
    .catch_unwind()
    .await;
    finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_shot_timeouts_quarantine_before_cancel_and_block_ordinary_active_requests() {
    let guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let body_result = std::panic::AssertUnwindSafe(async {
    for kind in [
        DeferredTimeoutKind::Send,
        DeferredTimeoutKind::Body,
        DeferredTimeoutKind::Sse,
    ] {
        let one_shot_workspace = unique_temp_workspace(&format!(
            "lingclaw-http-one-shot-timeout-before-cancel-{}",
            kind.label()
        ));
        let ordinary_workspace = unique_temp_workspace(&format!(
            "lingclaw-http-one-shot-timeout-ordinary-{}",
            kind.label()
        ));
        cleanup.track_path(one_shot_workspace.clone());
        cleanup.track_path(ordinary_workspace.clone());
        fs::create_dir_all(&one_shot_workspace).expect("create one-shot timeout workspace");
        fs::create_dir_all(&ordinary_workspace).expect("create ordinary sibling workspace");
        let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(server_task);
        state.block_cancellation.store(true, Ordering::Release);
        cleanup.track_barrier_release({
            let state = state.clone();
            move || {
                state.release_cancellation.notify_waiters();
                kind.release_origin(&state);
            }
        });

        let mut config = test_config_with_streamable_http_server(url);
        config
            .mcp_servers
            .get_mut("http")
            .expect("one-shot timeout server config")
            .timeout_secs = Some(1);
        let server = config
            .mcp_servers
            .get("http")
            .expect("one-shot timeout server");
        let domain_key = http_remote_cleanup_domain_key(server).expect("one-shot timeout domain");
        let session = TemporaryMcpSession::new("http", &config, &one_shot_workspace)
            .await
            .expect("one-shot Session must initialize before its request timeout");
        let one_shot_key = match &session {
            TemporaryMcpSession::Http(session) => session.cache_key.clone(),
            TemporaryMcpSession::Stdio(_) => panic!("expected HTTP one-shot Session"),
        };

        let ordinary_key = cache_key("http", server, &ordinary_workspace, &config)
            .expect("build ordinary sibling cache key");
        let ordinary_root = resolve_path_checked(".", &ordinary_workspace)
            .expect("resolve ordinary sibling workspace");
        set_http_session_id(
            &ordinary_key,
            Some(format!("ordinary-sibling-{}", kind.label())),
            &ordinary_root,
            true,
        )
        .expect("install ordinary sibling Session");
        http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock")
            .remote_domain_by_cache_key
            .insert(ordinary_key.clone(), domain_key.clone());

        let request = cleanup.spawn_worker({
            let one_shot_workspace = one_shot_workspace.clone();
            async move {
                let mut session = session;
                session
                    .request(&one_shot_workspace, kind.method(), json!({}))
                    .await
            }
        });
        tokio::time::timeout(Duration::from_secs(2), kind.started(&state).notified())
            .await
            .unwrap_or_else(|_| panic!("{} one-shot request must reach the server", kind.label()));
        tokio::time::timeout(
            Duration::from_secs(4),
            state.cancellation_started.notified(),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{} one-shot timeout must reach its cancellation barrier",
                kind.label()
            )
        });

        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain),
            "{} one-shot timeout must quarantine before cancellation returns",
            kind.label()
        );
        assert!(cached_http_session_identity_unchecked(&one_shot_key).is_none());
        let initialize_count = state.initialize_count.load(Ordering::Acquire);
        let ordinary_count = state.ordinary_request_count.load(Ordering::Acquire);
        assert_eq!(
            call_http_server("http", &config, &ordinary_workspace, "ping", json!({}))
                .await
                .expect_err("ordinary Active fast path must observe one-shot quarantine"),
            HTTP_MCP_CLEANUP_UNCERTAIN_ERROR
        );
        assert_eq!(
            state.initialize_count.load(Ordering::Acquire),
            initialize_count
        );
        assert_eq!(
            state.ordinary_request_count.load(Ordering::Acquire),
            ordinary_count,
            "the ordinary sibling must be rejected before network send"
        );

        state.release_cancellation.notify_waiters();
        kind.release_origin(&state);
        let error = tokio::time::timeout(Duration::from_secs(4), request)
            .await
            .expect("one-shot timeout request must finish")
            .expect("join one-shot timeout request")
            .expect_err("one-shot request must remain timed out");
        assert!(error.contains("timed out"), "{error}");
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain),
            "late one-shot lifecycle cleanup must not downgrade uncertainty"
        );
        assert_eq!(
            state.delete_count.load(Ordering::Acquire),
            0,
            "a timed-out one-shot request must not start a second cleanup under sticky uncertainty"
        );
        clear_mcp_caches_for_test().await;
    }
    })
    .catch_unwind()
    .await;
    finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_shot_initialize_timeout_quarantines_before_cancel_and_blocks_active_requests() {
    let guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let body_result = std::panic::AssertUnwindSafe(async {
        let ordinary_workspace = unique_temp_workspace("lingclaw-http-init-timeout-ordinary");
        let one_shot_workspace = unique_temp_workspace("lingclaw-http-init-timeout-one-shot");
        cleanup.track_path(ordinary_workspace.clone());
        cleanup.track_path(one_shot_workspace.clone());
        fs::create_dir_all(&ordinary_workspace)
            .expect("create ordinary initialize-timeout workspace");
        fs::create_dir_all(&one_shot_workspace)
            .expect("create one-shot initialize-timeout workspace");
        let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(server_task);
        let mut config = test_config_with_streamable_http_server(url);
        config
            .mcp_servers
            .get_mut("http")
            .expect("initialize-timeout server config")
            .timeout_secs = Some(1);
        let server = config
            .mcp_servers
            .get("http")
            .expect("initialize-timeout server");
        let domain_key =
            http_remote_cleanup_domain_key(server).expect("initialize-timeout cleanup domain");

        call_http_server("http", &config, &ordinary_workspace, "ping", json!({}))
            .await
            .expect("warm ordinary Session before one-shot initialize timeout");
        state.block_initialize.store(true, Ordering::Release);
        state.block_cancellation.store(true, Ordering::Release);
        cleanup.track_barrier_release({
            let state = state.clone();
            move || {
                state.release_cancellation.notify_waiters();
                state.release_initialize.notify_waiters();
            }
        });
        let constructor = cleanup.spawn_worker({
            let config = config.clone();
            let one_shot_workspace = one_shot_workspace.clone();
            async move { TemporaryMcpSession::new("http", &config, &one_shot_workspace).await }
        });
        tokio::time::timeout(Duration::from_secs(2), state.initialize_started.notified())
            .await
            .expect("one-shot initialize must reach the server");
        tokio::time::timeout(
            Duration::from_secs(4),
            state.cancellation_started.notified(),
        )
        .await
        .expect("initialize timeout must reach its cancellation barrier");

        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain),
            "initialize timeout must quarantine before cancellation returns"
        );
        let initialize_count = state.initialize_count.load(Ordering::Acquire);
        let ordinary_count = state.ordinary_request_count.load(Ordering::Acquire);
        assert_eq!(
            call_http_server("http", &config, &ordinary_workspace, "ping", json!({}))
                .await
                .expect_err("ordinary Active Session must observe initialize quarantine"),
            HTTP_MCP_CLEANUP_UNCERTAIN_ERROR
        );
        assert_eq!(
            state.initialize_count.load(Ordering::Acquire),
            initialize_count
        );
        assert_eq!(
            state.ordinary_request_count.load(Ordering::Acquire),
            ordinary_count
        );

        state.release_cancellation.notify_waiters();
        state.release_initialize.notify_waiters();
        let result = tokio::time::timeout(Duration::from_secs(4), constructor)
            .await
            .expect("one-shot constructor must finish after cancellation release")
            .expect("join one-shot initialize timeout");
        let error = match result {
            Ok(_) => panic!("one-shot initialize must remain timed out"),
            Err(error) => error,
        };
        assert!(error.contains("timed out"), "{error}");
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain)
        );
        assert_eq!(
            state.delete_count.load(Ordering::Acquire),
            0,
            "an initialize timeout with no known Session ID must not invent a DELETE"
        );
        invalidate_runtime_state_without_remote_shutdown().await;
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain),
            "Settings invalidation must not clear a sticky initialize-timeout tombstone"
        );
        {
            let runtime = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            assert_eq!(runtime.cleanups.len(), 1);
            assert!(runtime.cleanup_tasks.is_empty());
            assert!(runtime.in_flight_requests.is_empty());
        }

        clear_mcp_caches_for_test().await;
    })
    .catch_unwind()
    .await;
    finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panic_safe_mcp_fixture_awaits_failed_workers_before_clearing_state_and_paths() {
    let ordinary_workspace = unique_temp_workspace("lingclaw-mcp-panic-safe-ordinary");
    let caller_cancel_workspace = unique_temp_workspace("lingclaw-mcp-panic-safe-caller-cancel");
    let body_timeout_workspace = unique_temp_workspace("lingclaw-mcp-panic-safe-body-timeout");
    let blocked_workspace = unique_temp_workspace("lingclaw-mcp-panic-safe-blocked");
    let mut timed_out_probe = None;
    let mut pending_cleanup_probe = None;
    let mut caller_cancel_probe = None;
    let mut body_timeout_probe = None;
    let mut body_timeout_second_probe = None;
    let mut blocked_probe = None;
    let ordinary_panic = std::panic::AssertUnwindSafe(async {
        let guard = acquire_mcp_test_guard().await;
        clear_mcp_caches_for_test().await;
        let mut cleanup = PanicSafeMcpFixture::default();
        let body_result = std::panic::AssertUnwindSafe(async {
            cleanup.track_path(ordinary_workspace.clone());
            cleanup.track_path(caller_cancel_workspace.clone());
            cleanup.track_path(body_timeout_workspace.clone());
            cleanup.track_path(blocked_workspace.clone());
            fs::create_dir_all(&ordinary_workspace).expect("create panic-safe ordinary workspace");
            fs::create_dir_all(&caller_cancel_workspace)
                .expect("create panic-safe caller-cancel workspace");
            fs::create_dir_all(&body_timeout_workspace)
                .expect("create panic-safe body-timeout workspace");
            fs::create_dir_all(&blocked_workspace).expect("create panic-safe blocked workspace");
            let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
            cleanup.track_task(server_task);
            state.block_cancellation.store(true, Ordering::Release);
            cleanup.track_barrier_release({
                let state = state.clone();
                move || {
                    state.release_delete.notify_waiters();
                    state.release_cancellation.notify_waiters();
                    state.release_send_timeout.notify_waiters();
                }
            });

            let mut config = test_config_with_streamable_http_server(url);
            config
                .mcp_servers
                .get_mut("http")
                .expect("panic-safe ordinary server")
                .timeout_secs = Some(1);
            let server = config
                .mcp_servers
                .get("http")
                .expect("panic-safe ordinary server");
            let domain_key =
                http_remote_cleanup_domain_key(server).expect("panic-safe ordinary cleanup domain");
            call_http_server("http", &config, &ordinary_workspace, "ping", json!({}))
                .await
                .expect("warm panic-safe ordinary Session");

            let cleanup_key = "http\npanic-safe-pending-cleanup".to_string();
            let workspace_root = resolve_path_checked(".", &ordinary_workspace)
                .expect("resolve panic-safe ordinary workspace");
            set_http_session_id(
                &cleanup_key,
                Some("panic-safe-pending-delete".to_string()),
                &workspace_root,
                true,
            )
            .expect("install panic-safe pending cleanup Session");
            http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock")
                .remote_domain_by_cache_key
                .insert(cleanup_key.clone(), domain_key);

            let (_, probe) = cleanup.spawn_worker_with_probe({
                let config = config.clone();
                let workspace = ordinary_workspace.clone();
                async move {
                    call_http_server(
                        "http",
                        &config,
                        &workspace,
                        DeferredTimeoutKind::Send.method(),
                        json!({}),
                    )
                    .await
                }
            });
            timed_out_probe = Some(probe);
            tokio::time::timeout(
                Duration::from_secs(2),
                state.send_timeout_started.notified(),
            )
            .await
            .expect("panic-safe ordinary worker must reach its barrier");

            let (_, probe) = cleanup.spawn_worker_with_probe({
                let server = server.clone();
                async move { terminate_http_session("http", &cleanup_key, &server).await }
            });
            pending_cleanup_probe = Some(probe);
            tokio::time::timeout(Duration::from_secs(2), state.delete_started.notified())
                .await
                .expect("panic-safe cleanup worker must reach its barrier");

            tokio::time::timeout(
                Duration::from_secs(4),
                state.cancellation_started.notified(),
            )
            .await
            .expect("panic-safe cancellation-timeout worker must reach its barrier");

            let (caller_url, caller_state, caller_server_task) =
                spawn_deferred_cleanup_http_test_server().await;
            cleanup.track_task(caller_server_task);
            caller_state
                .block_cancellation
                .store(true, Ordering::Release);
            cleanup.track_barrier_release({
                let state = caller_state.clone();
                move || {
                    state.release_cancellation.notify_waiters();
                    state.release_send_timeout.notify_waiters();
                }
            });
            let mut caller_config = test_config_with_streamable_http_server(caller_url);
            caller_config
                .mcp_servers
                .get_mut("http")
                .expect("panic-safe caller-cancel server")
                .timeout_secs = Some(1);
            call_http_server(
                "http",
                &caller_config,
                &caller_cancel_workspace,
                "ping",
                json!({}),
            )
            .await
            .expect("warm panic-safe caller-cancel Session");
            let (caller_cancel, probe) = cleanup.spawn_worker_with_probe({
                let config = caller_config.clone();
                let workspace = caller_cancel_workspace.clone();
                async move {
                    call_http_server(
                        "http",
                        &config,
                        &workspace,
                        DeferredTimeoutKind::Send.method(),
                        json!({}),
                    )
                    .await
                }
            });
            caller_cancel_probe = Some(probe);
            tokio::time::timeout(
                Duration::from_secs(2),
                caller_state.send_timeout_started.notified(),
            )
            .await
            .expect("panic-safe caller-cancel worker must reach the request barrier");
            tokio::time::timeout(
                Duration::from_secs(4),
                caller_state.cancellation_started.notified(),
            )
            .await
            .expect("panic-safe caller-cancel worker must reach the cancellation barrier");
            caller_cancel.abort();

            let (body_url, body_state, body_server_task) =
                spawn_deferred_cleanup_http_test_server().await;
            cleanup.track_task(body_server_task);
            cleanup.track_barrier_release({
                let state = body_state.clone();
                move || state.release_second_request.notify_waiters()
            });
            let mut body_config = test_config_with_streamable_http_server(body_url);
            body_config
                .mcp_servers
                .get_mut("http")
                .expect("panic-safe body-timeout server")
                .timeout_secs = Some(30);
            call_http_server(
                "http",
                &body_config,
                &body_timeout_workspace,
                "ping",
                json!({}),
            )
            .await
            .expect("warm panic-safe body-timeout Session");
            let (_, probe) = cleanup.spawn_worker_with_probe({
                let config = body_config.clone();
                let workspace = body_timeout_workspace.clone();
                async move {
                    call_http_server(
                        "http",
                        &config,
                        &workspace,
                        "deferred/body-timeout",
                        json!({}),
                    )
                    .await
                }
            });
            body_timeout_probe = Some(probe);
            tokio::time::timeout(
                Duration::from_secs(2),
                body_state.body_timeout_started.notified(),
            )
            .await
            .expect("panic-safe body-timeout worker must reach its body barrier");
            let (_, probe) = cleanup.spawn_worker_with_probe({
                let config = body_config.clone();
                let workspace = body_timeout_workspace.clone();
                async move {
                    call_http_server("http", &config, &workspace, "deferred/second", json!({}))
                        .await
                }
            });
            body_timeout_second_probe = Some(probe);
            tokio::time::timeout(
                Duration::from_secs(2),
                body_state.second_request_started.notified(),
            )
            .await
            .expect("panic-safe body-timeout second worker must reach its barrier");

            let (blocked_url, blocked_state, blocked_server_task) =
                spawn_same_id_reuse_http_test_server().await;
            cleanup.track_task(blocked_server_task);
            cleanup.track_barrier_release({
                let state = blocked_state.clone();
                move || state.release_old_response.notify_waiters()
            });
            let blocked_config = test_config_with_streamable_http_server(blocked_url);
            call_http_server(
                "http",
                &blocked_config,
                &blocked_workspace,
                "ping",
                json!({}),
            )
            .await
            .expect("warm panic-safe blocked Session");
            let (_, probe) = cleanup.spawn_worker_with_probe({
                let config = blocked_config.clone();
                let workspace = blocked_workspace.clone();
                async move {
                    call_http_server("http", &config, &workspace, "tools/list", json!({})).await
                }
            });
            blocked_probe = Some(probe);
            tokio::time::timeout(
                Duration::from_secs(2),
                blocked_state.old_response_started.notified(),
            )
            .await
            .expect("panic-safe ordinary blocked worker must reach its barrier");

            panic!("intentional ordinary MCP fixture failure");
        })
        .catch_unwind()
        .await;
        finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
    })
    .catch_unwind()
    .await;
    assert!(
        ordinary_panic.is_err(),
        "the controlled failure must unwind"
    );
    assert!(
        timed_out_probe
            .as_ref()
            .is_some_and(|probe| probe.completed.load(Ordering::Acquire)),
        "the timed-out worker must finish dropping before cleanup returns"
    );
    assert!(
        pending_cleanup_probe
            .as_ref()
            .is_some_and(|probe| probe.completed.load(Ordering::Acquire)),
        "the pending cleanup worker must finish dropping before cleanup returns"
    );
    for (probe, label) in [
        (&caller_cancel_probe, "caller-cancel"),
        (&body_timeout_probe, "body-timeout"),
        (&body_timeout_second_probe, "body-timeout second"),
        (&blocked_probe, "ordinary blocked"),
    ] {
        assert!(
            probe
                .as_ref()
                .is_some_and(|probe| probe.completed.load(Ordering::Acquire)),
            "the {label} worker must finish dropping before cleanup returns"
        );
    }
    for path in [
        &ordinary_workspace,
        &caller_cancel_workspace,
        &body_timeout_workspace,
        &blocked_workspace,
    ] {
        assert!(
            !path
                .try_exists()
                .expect("check panic-safe ordinary workspace removal"),
            "{} must be removed after worker teardown",
            path.display()
        );
    }

    let one_shot_workspace = unique_temp_workspace("lingclaw-mcp-panic-safe-one-shot");
    let constructor_workspace = unique_temp_workspace("lingclaw-mcp-panic-safe-constructor");
    let mut request_probe = None;
    let mut constructor_probe = None;
    let one_shot_panic = std::panic::AssertUnwindSafe(async {
        let guard = acquire_mcp_test_guard().await;
        assert_mcp_test_state_empty().expect("ordinary panic cleanup must leave no state");
        let mut cleanup = PanicSafeMcpFixture::default();
        let body_result = std::panic::AssertUnwindSafe(async {
            cleanup.track_path(one_shot_workspace.clone());
            cleanup.track_path(constructor_workspace.clone());
            fs::create_dir_all(&one_shot_workspace).expect("create panic-safe one-shot workspace");
            fs::create_dir_all(&constructor_workspace)
                .expect("create panic-safe constructor workspace");

            let (request_url, request_state, request_server_task) =
                spawn_deferred_cleanup_http_test_server().await;
            cleanup.track_task(request_server_task);
            request_state
                .block_cancellation
                .store(true, Ordering::Release);
            cleanup.track_barrier_release({
                let state = request_state.clone();
                move || {
                    state.release_cancellation.notify_waiters();
                    state.release_send_timeout.notify_waiters();
                }
            });
            let mut request_config = test_config_with_streamable_http_server(request_url);
            request_config
                .mcp_servers
                .get_mut("http")
                .expect("panic-safe request server")
                .timeout_secs = Some(1);
            let session = TemporaryMcpSession::new("http", &request_config, &one_shot_workspace)
                .await
                .expect("initialize panic-safe one-shot Session");
            let (_, probe) = cleanup.spawn_worker_with_probe({
                let workspace = one_shot_workspace.clone();
                async move {
                    let mut session = session;
                    session
                        .request(&workspace, DeferredTimeoutKind::Send.method(), json!({}))
                        .await
                }
            });
            request_probe = Some(probe);
            tokio::time::timeout(
                Duration::from_secs(2),
                request_state.send_timeout_started.notified(),
            )
            .await
            .expect("panic-safe one-shot request must reach its barrier");

            let (constructor_url, constructor_state, constructor_server_task) =
                spawn_deferred_cleanup_http_test_server().await;
            cleanup.track_task(constructor_server_task);
            constructor_state
                .block_initialize
                .store(true, Ordering::Release);
            constructor_state
                .block_cancellation
                .store(true, Ordering::Release);
            cleanup.track_barrier_release({
                let state = constructor_state.clone();
                move || {
                    state.release_initialize.notify_waiters();
                    state.release_cancellation.notify_waiters();
                }
            });
            let mut constructor_config = test_config_with_streamable_http_server(constructor_url);
            constructor_config
                .mcp_servers
                .get_mut("http")
                .expect("panic-safe constructor server")
                .timeout_secs = Some(1);
            let (_, probe) =
                cleanup.spawn_worker_with_probe({
                    let workspace = constructor_workspace.clone();
                    async move {
                        TemporaryMcpSession::new("http", &constructor_config, &workspace).await
                    }
                });
            constructor_probe = Some(probe);
            tokio::time::timeout(
                Duration::from_secs(2),
                constructor_state.initialize_started.notified(),
            )
            .await
            .expect("panic-safe constructor must reach its barrier");

            panic!("intentional one-shot MCP fixture failure");
        })
        .catch_unwind()
        .await;
        finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
    })
    .catch_unwind()
    .await;
    assert!(
        one_shot_panic.is_err(),
        "the controlled failure must unwind"
    );
    assert!(
        request_probe
            .as_ref()
            .is_some_and(|probe| probe.completed.load(Ordering::Acquire)),
        "the one-shot request worker must finish dropping before cleanup returns"
    );
    assert!(
        constructor_probe
            .as_ref()
            .is_some_and(|probe| probe.completed.load(Ordering::Acquire)),
        "the one-shot constructor worker must finish dropping before cleanup returns"
    );
    for path in [&one_shot_workspace, &constructor_workspace] {
        assert!(
            !path
                .try_exists()
                .expect("check panic-safe one-shot workspace removal"),
            "{} must be removed after worker teardown",
            path.display()
        );
    }

    let successor_workspace = unique_temp_workspace("lingclaw-mcp-panic-safe-successor");
    let guard = acquire_mcp_test_guard().await;
    tokio::task::yield_now().await;
    assert_mcp_test_state_empty().expect("failed workers must not write state after lock release");
    let mut cleanup = PanicSafeMcpFixture::default();
    let body_result = std::panic::AssertUnwindSafe(async {
        cleanup.track_path(successor_workspace.clone());
        fs::create_dir_all(&successor_workspace).expect("create successor MCP workspace");
        let (url, _, server_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(server_task);
        let config = test_config_with_streamable_http_server(url);
        call_http_server("http", &config, &successor_workspace, "ping", json!({}))
            .await
            .expect("a successor MCP test must not observe late failed-worker state");
    })
    .catch_unwind()
    .await;
    finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
    assert!(
        !successor_workspace
            .try_exists()
            .expect("check successor MCP workspace removal")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panic_safe_mcp_finish_preserves_body_panic_when_cleanup_also_fails() {
    #[derive(Debug, PartialEq, Eq)]
    struct OriginalPanic(&'static str);

    let cleanup_only = std::panic::AssertUnwindSafe(async {
        let guard = acquire_mcp_test_guard().await;
        clear_mcp_caches_for_test().await;
        let mut cleanup = PanicSafeMcpFixture::default();
        cleanup.force_state_cleanup_error("forced cleanup-only state failure");
        finish_panic_safe_mcp_test(guard, cleanup, Ok(())).await;
    })
    .catch_unwind()
    .await
    .expect_err("a cleanup-only failure must fail the test");
    let cleanup_only = cleanup_only
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| cleanup_only.downcast_ref::<&'static str>().copied())
        .expect("cleanup-only failure should use a string panic");
    assert!(
        cleanup_only.contains("forced cleanup-only state failure"),
        "{cleanup_only}"
    );

    let workspace = unique_temp_workspace("lingclaw-mcp-panic-and-cleanup-failure");
    let diagnostics = Arc::new(std::sync::Mutex::new(Vec::new()));
    let original = std::panic::AssertUnwindSafe(async {
        let guard = acquire_mcp_test_guard().await;
        assert_mcp_test_state_empty().expect("cleanup-only failure must still clear state");
        let mut cleanup = PanicSafeMcpFixture::default();
        cleanup.cleanup_diagnostics = diagnostics.clone();
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create dual-failure MCP workspace");
        cleanup.force_state_cleanup_error("forced dual-failure state cleanup error");
        cleanup
            .force_path_cleanup_error(workspace.clone(), "forced dual-failure path cleanup error");
        let body_result = std::panic::catch_unwind(|| {
            std::panic::panic_any(OriginalPanic("original MCP test panic"));
        });
        finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
    })
    .catch_unwind()
    .await
    .expect_err("the original panic must be resumed");
    let original = original
        .downcast::<OriginalPanic>()
        .expect("cleanup failure must not replace the original panic payload");
    assert_eq!(*original, OriginalPanic("original MCP test panic"));
    assert!(
        !workspace
            .try_exists()
            .expect("check dual-failure workspace removal"),
        "path cleanup must still run before the original panic resumes"
    );
    let diagnostics = diagnostics
        .lock()
        .expect("cleanup diagnostics lock")
        .clone();
    assert_eq!(diagnostics.len(), 1);
    assert!(
        diagnostics[0].contains("forced dual-failure state cleanup error"),
        "{}",
        diagnostics[0]
    );
    assert!(
        diagnostics[0].contains("forced dual-failure path cleanup error"),
        "{}",
        diagnostics[0]
    );

    let successor_workspace = unique_temp_workspace("lingclaw-mcp-dual-failure-successor");
    let guard = acquire_mcp_test_guard().await;
    assert_mcp_test_state_empty().expect("dual failure must release the lock and clear state");
    let mut cleanup = PanicSafeMcpFixture::default();
    let body_result = std::panic::AssertUnwindSafe(async {
        cleanup.track_path(successor_workspace.clone());
        fs::create_dir_all(&successor_workspace).expect("create dual-failure successor workspace");
        let (url, _, server_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(server_task);
        let config = test_config_with_streamable_http_server(url);
        call_http_server("http", &config, &successor_workspace, "ping", json!({}))
            .await
            .expect("a successor call must work after the dual failure");
    })
    .catch_unwind()
    .await;
    finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
    assert!(
        !successor_workspace
            .try_exists()
            .expect("check dual-failure successor workspace removal")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_notification_timeout_cannot_release_ordinary_timeout_quarantine() {
    let guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let body_result = std::panic::AssertUnwindSafe(async {
        let workspace = unique_temp_workspace("lingclaw-http-cancel-notification-timeout");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create cancellation-timeout workspace");
        let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(server_task);
        state.block_cancellation.store(true, Ordering::Release);
        cleanup.track_barrier_release({
            let state = state.clone();
            move || {
                state.release_cancellation.notify_waiters();
                state.release_send_timeout.notify_waiters();
            }
        });
        let mut config = test_config_with_streamable_http_server(url);
        config
            .mcp_servers
            .get_mut("http")
            .expect("cancellation-timeout server config")
            .timeout_secs = Some(1);
        let server = config
            .mcp_servers
            .get("http")
            .expect("cancellation-timeout server");
        let cache_key =
            cache_key("http", server, &workspace, &config).expect("cancellation-timeout cache key");
        let domain_key =
            http_remote_cleanup_domain_key(server).expect("cancellation-timeout domain");

        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm cancellation-timeout Session");
        let initialize_count = state.initialize_count.load(Ordering::Acquire);
        let request = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server(
                    "http",
                    &config,
                    &workspace,
                    "deferred/send-timeout",
                    json!({}),
                )
                .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            state.send_timeout_started.notified(),
        )
        .await
        .expect("send-timeout request must reach the server");
        tokio::time::timeout(
            Duration::from_secs(4),
            state.cancellation_started.notified(),
        )
        .await
        .expect("cancellation request must reach its blocking handler");
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain)
        );

        let error = tokio::time::timeout(Duration::from_secs(4), request)
            .await
            .expect("best-effort cancellation timeout must remain bounded")
            .expect("join cancellation-timeout request")
            .expect_err("original request must remain a timeout");
        assert!(error.contains("timed out"), "{error}");
        assert_eq!(state.cancellation_count.load(Ordering::Acquire), 1);
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain),
            "cancellation timeout must not clear endpoint quarantine"
        );
        assert!(cached_http_session_identity_unchecked(&cache_key).is_none());
        assert_eq!(
            call_http_server("http", &config, &workspace, "ping", json!({}))
                .await
                .expect_err("cancellation timeout must block reinitialize"),
            HTTP_MCP_CLEANUP_UNCERTAIN_ERROR
        );
        assert_eq!(
            state.initialize_count.load(Ordering::Acquire),
            initialize_count
        );

        state.release_cancellation.notify_waiters();
        state.release_send_timeout.notify_waiters();
    })
    .catch_unwind()
    .await;
    finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_notification_connection_error_keeps_ordinary_timeout_quarantined() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-cancel-notification-connection-error");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create cancellation-connection-error workspace");
    let reset_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind cancellation-connection-error listener");
    let reset_addr = reset_listener
        .local_addr()
        .expect("read cancellation-connection-error address");
    cleanup.track_task(tokio::spawn(async move {
        if let Ok((connection, _)) = reset_listener.accept().await {
            drop(connection);
        }
    }));
    let config = test_config_with_streamable_http_server(format!("http://{reset_addr}/"));
    let server = config
        .mcp_servers
        .get("http")
        .expect("cancellation-connection-error server");
    let cache_key = cache_key("http", server, &workspace, &config)
        .expect("cancellation-connection-error cache key");
    let domain_key =
        http_remote_cleanup_domain_key(server).expect("cancellation-connection-error domain");
    let checked =
        resolve_path_checked(".", &workspace).expect("resolve connection-error workspace");
    let identity = set_http_session_id(
        &cache_key,
        Some("connection-error-session".to_string()),
        &checked,
        true,
    )
    .expect("seed connection-error Session");
    http_runtime_state()
        .lock()
        .expect("HTTP runtime state lock")
        .remote_domain_by_cache_key
        .insert(cache_key.clone(), domain_key.clone());
    let mut authority =
        capture_http_request_authority(&cache_key, Some(identity.session_id.as_str()), false)
            .expect("capture connection-error request authority");

    fail_closed_http_timeout_before_cancel(
        &cache_key,
        server,
        Some(&identity),
        None,
        &mut authority,
    );
    assert_eq!(
        http_cleanup_phase(&domain_key),
        Some(HttpCleanupPhase::Uncertain)
    );
    assert!(cached_http_session_identity_unchecked(&cache_key).is_none());
    assert!(
        http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock")
            .in_flight_requests
            .is_empty()
    );

    tokio::time::timeout(
        Duration::from_secs(1),
        send_http_cancelled_notification(
            "http",
            server,
            Some(identity.session_id.as_str()),
            json!(17),
            "request timed out",
        ),
    )
    .await
    .expect("connection-refused cancellation should return promptly");
    assert_eq!(
        http_cleanup_phase(&domain_key),
        Some(HttpCleanupPhase::Uncertain),
        "cancellation connection failure must not clear quarantine"
    );
    assert!(cached_http_session_identity_unchecked(&cache_key).is_none());
    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelling_timeout_caller_keeps_quarantine_and_cannot_remove_replacement() {
    let guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let body_result = std::panic::AssertUnwindSafe(async {
        let workspace = unique_temp_workspace("lingclaw-http-cancel-timeout-caller");
        let replacement_workspace =
            unique_temp_workspace("lingclaw-http-cancel-timeout-caller-replacement");
        cleanup.track_path(workspace.clone());
        cleanup.track_path(replacement_workspace.clone());
        fs::create_dir_all(&workspace).expect("create cancelled-timeout workspace");
        fs::create_dir_all(&replacement_workspace).expect("create replacement workspace");
        let (url, state, server_task) = spawn_deferred_cleanup_http_test_server().await;
        cleanup.track_task(server_task);
        state.block_cancellation.store(true, Ordering::Release);
        cleanup.track_barrier_release({
            let state = state.clone();
            move || {
                state.release_cancellation.notify_waiters();
                state.release_send_timeout.notify_waiters();
            }
        });
        let mut config = test_config_with_streamable_http_server(url);
        config
            .mcp_servers
            .get_mut("http")
            .expect("cancelled-timeout server config")
            .timeout_secs = Some(1);
        let server = config
            .mcp_servers
            .get("http")
            .expect("cancelled-timeout server");
        let domain_key = http_remote_cleanup_domain_key(server).expect("cancelled-timeout domain");

        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm cancelled-timeout Session");
        let request = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move {
                call_http_server(
                    "http",
                    &config,
                    &workspace,
                    "deferred/send-timeout",
                    json!({}),
                )
                .await
            }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            state.send_timeout_started.notified(),
        )
        .await
        .expect("cancelled timeout request must reach the server");
        tokio::time::timeout(
            Duration::from_secs(4),
            state.cancellation_started.notified(),
        )
        .await
        .expect("cancelled timeout must reach its cancellation barrier");
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain)
        );

        let mut replacement_config = config.clone();
        replacement_config
            .mcp_servers
            .get_mut("http")
            .expect("replacement server config")
            .timeout_secs = Some(19);
        let replacement_server = replacement_config
            .mcp_servers
            .get("http")
            .expect("replacement server");
        let replacement_key = cache_key(
            "http",
            replacement_server,
            &replacement_workspace,
            &replacement_config,
        )
        .expect("replacement cache key");
        let replacement_root = resolve_path_checked(".", &replacement_workspace)
            .expect("resolve replacement workspace");
        let replacement_identity = set_http_session_id(
            &replacement_key,
            Some("deferred-fixed-session".to_string()),
            &replacement_root,
            true,
        )
        .expect("install same-ID replacement generation");
        http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock")
            .remote_domain_by_cache_key
            .insert(replacement_key.clone(), domain_key.clone());

        request.abort();
        assert!(
            request
                .await
                .expect_err("timeout caller must be cancelled")
                .is_cancelled()
        );
        assert!(http_session_identity_is_current(
            &replacement_key,
            &replacement_identity
        ));
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain),
            "caller cancellation must retain endpoint quarantine"
        );
        assert_eq!(state.cancellation_count.load(Ordering::Acquire), 1);
        assert_eq!(state.delete_count.load(Ordering::Acquire), 0);

        state.release_cancellation.notify_waiters();
        state.release_send_timeout.notify_waiters();
        remove_http_session(&replacement_key);
    })
    .catch_unwind()
    .await;
    finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_and_panicking_ordinary_requests_release_only_their_generation() {
    let guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let body_result = std::panic::AssertUnwindSafe(async {
        let workspace = unique_temp_workspace("lingclaw-http-inflight-drop");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create in-flight Drop workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_old_response.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("in-flight Drop server");
        let cache_key =
            cache_key("http", server, &workspace, &config).expect("build in-flight Drop cache key");

        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm in-flight Drop Session");
        let blocked =
            cleanup.spawn_worker({
                let config = config.clone();
                let workspace = workspace.clone();
                async move {
                    call_http_server("http", &config, &workspace, "tools/list", json!({})).await
                }
            });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.old_response_started.notified(),
        )
        .await
        .expect("ordinary request must reach its cancellation barrier");
        let old_identity = cached_http_session_identity_unchecked(&cache_key)
            .expect("ordinary request identity should remain cached");
        blocked.abort();
        assert!(
            blocked
                .await
                .expect_err("ordinary request task should be cancelled")
                .is_cancelled()
        );
        assert!(
            http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock")
                .in_flight_requests
                .is_empty(),
            "task cancellation must drop its exact in-flight lease"
        );
        server_state.release_old_response.notify_waiters();

        let old_key = HttpInFlightRequestKey {
            cache_key: cache_key.clone(),
            epoch: old_identity.epoch,
            generation: old_identity.generation,
        };
        let replacement_key = HttpInFlightRequestKey {
            cache_key: cache_key.clone(),
            epoch: old_identity.epoch,
            generation: old_identity.generation + 1,
        };
        {
            let mut state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            state.in_flight_requests.insert(old_key.clone(), 1);
            state.in_flight_requests.insert(replacement_key.clone(), 1);
        }
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _lease = HttpInFlightRequestLease {
                key: old_key.clone(),
                completed: false,
            };
            panic!("intentional in-flight lease panic");
        }));
        assert!(panic.is_err());
        {
            let mut state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            assert!(!state.in_flight_requests.contains_key(&old_key));
            assert_eq!(state.in_flight_requests.get(&replacement_key), Some(&1));
            state.in_flight_requests.remove(&replacement_key);
        }
    })
    .catch_unwind()
    .await;
    finish_panic_safe_mcp_test(guard, cleanup, body_result).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timed_out_ordinary_request_releases_its_lease_without_late_cache_revival() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-inflight-timeout");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create in-flight timeout workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_old_response.notify_waiters()
        });
        let mut config = test_config_with_streamable_http_server(url);
        config
            .mcp_servers
            .get_mut("http")
            .expect("in-flight timeout server")
            .timeout_secs = Some(1);
        let server = config
            .mcp_servers
            .get("http")
            .expect("in-flight timeout server");
        let cache_key = cache_key("http", server, &workspace, &config)
            .expect("build in-flight timeout cache key");

        call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect("warm in-flight timeout Session");
        let request =
            cleanup.spawn_worker({
                let config = config.clone();
                let workspace = workspace.clone();
                async move {
                    call_http_server("http", &config, &workspace, "tools/list", json!({})).await
                }
            });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.old_response_started.notified(),
        )
        .await
        .expect("ordinary timeout request must reach the server");
        let error = tokio::time::timeout(Duration::from_secs(3), request)
            .await
            .expect("ordinary request timeout must be bounded")
            .expect("join timed-out ordinary request")
            .expect_err("blocked ordinary request must time out");
        assert!(error.contains("timed out"), "{error}");
        assert!(
            http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock")
                .in_flight_requests
                .is_empty()
        );
        assert!(cached_http_session_identity_unchecked(&cache_key).is_none());
        let initialize_count = server_state.next_generation.load(Ordering::Acquire);
        server_state.release_old_response.notify_waiters();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(cached_http_session_identity_unchecked(&cache_key).is_none());
        assert_eq!(
            server_state.next_generation.load(Ordering::Acquire),
            initialize_count,
            "a late ordinary response must not retry or resurrect a local Session"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_delete_statuses_release_one_shot_scope_for_reuse() {
    run_panic_safe_mcp_test!(cleanup, {
        for status in [StatusCode::OK, StatusCode::NO_CONTENT] {
            let workspace = unique_temp_workspace(&format!(
                "lingclaw-http-one-shot-terminal-delete-{}",
                status.as_u16()
            ));
            cleanup.track_path(workspace.clone());
            fs::create_dir_all(&workspace).expect("create terminal DELETE workspace");
            let (url, server_state, server_task) =
                spawn_same_id_reuse_http_test_server_with_delete(status, false).await;
            cleanup.track_task(server_task);
            cleanup.track_barrier_release({
                let server_state = server_state.clone();
                move || server_state.release_delete.notify_waiters()
            });
            let config = test_config_with_streamable_http_server(url);

            for generation in 0..2 {
                let mut session = TemporaryMcpSession::new("http", &config, &workspace)
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "{status} cleanup should allow one-shot generation {generation}: {error}"
                    )
                });
                let cleanup_caller = cleanup.spawn_worker(async move { session.shutdown().await });
                tokio::time::timeout(
                    Duration::from_secs(2),
                    server_state.delete_started.notified(),
                )
                .await
                .expect("terminal DELETE should reach the server");
                server_state.release_delete.notify_one();
                cleanup_caller
                    .await
                    .expect("join terminal one-shot cleanup");
            }

            assert_eq!(
                server_state.active_generation.load(Ordering::Acquire),
                0,
                "{status} should finish both remote Session generations"
            );
            clear_mcp_caches_for_test().await;
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_shot_cleanup_domain_survives_local_config_and_client_scope_changes() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-one-shot-domain");
    let alternate_workspace = unique_temp_workspace("lingclaw-http-one-shot-domain-alt");
    cleanup.track_path(workspace.clone());
    cleanup.track_path(alternate_workspace.clone());
    fs::create_dir_all(&workspace).expect("create one-shot domain workspace");
    fs::create_dir_all(&alternate_workspace).expect("create alternate one-shot domain workspace");
    let (url, server_state, server_task) =
        spawn_same_id_reuse_http_test_server_with_delete(StatusCode::ACCEPTED, true).await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);

    let mut first = TemporaryMcpSession::new("http", &config, &workspace)
        .await
        .expect("first one-shot Session should initialize");
    first.shutdown().await;
    let initialize_count_after_first = server_state.next_generation.load(Ordering::Acquire);

    let mut timeout_config = config.clone();
    let timeout_server = timeout_config
        .mcp_servers
        .get_mut("http")
        .expect("timeout server config");
    timeout_server.timeout_secs = Some(19);

    let mut ignored_stdio_config = config.clone();
    let ignored_stdio_server = ignored_stdio_config
        .mcp_servers
        .get_mut("http")
        .expect("ignored stdio server config");
    ignored_stdio_server.command = "ignored-command".to_string();
    ignored_stdio_server.args = vec!["ignored-arg".to_string()];
    ignored_stdio_server
        .env
        .insert("IGNORED_HTTP_ENV".to_string(), "changed".to_string());
    ignored_stdio_server.cwd = Some(".".to_string());

    let mut header_config = config.clone();
    header_config
        .mcp_servers
        .get_mut("http")
        .expect("header server config")
        .headers
        .insert(
            "x-api-key".to_string(),
            "rotated-local-credential".to_string(),
        );

    let mut fragment_config = config.clone();
    let fragment_url = fragment_config
        .mcp_servers
        .get("http")
        .and_then(|server| server.url.clone())
        .expect("fragment server url");
    fragment_config
        .mcp_servers
        .get_mut("http")
        .expect("fragment server config")
        .url = Some(format!("{fragment_url}#local-fragment"));

    let mut userinfo_config = config.clone();
    let mut userinfo_url = reqwest::Url::parse(
        userinfo_config
            .mcp_servers
            .get("http")
            .and_then(|server| server.url.as_deref())
            .expect("userinfo server url"),
    )
    .expect("parse userinfo server url");
    userinfo_url
        .set_username("rotated-user")
        .expect("set URL username");
    userinfo_url
        .set_password(Some("rotated-password"))
        .expect("set URL password");
    userinfo_config
        .mcp_servers
        .get_mut("http")
        .expect("userinfo server config")
        .url = Some(userinfo_url.to_string());

    let mut alias_config = config.clone();
    let alias_server = alias_config
        .mcp_servers
        .get("http")
        .expect("aliased server config")
        .clone();
    alias_config
        .mcp_servers
        .insert("http-alias".to_string(), alias_server);

    let attempts = [
        TemporaryMcpSession::new("http", &timeout_config, &workspace).await,
        TemporaryMcpSession::new("http", &ignored_stdio_config, &workspace).await,
        TemporaryMcpSession::new("http", &header_config, &workspace).await,
        TemporaryMcpSession::new("http", &fragment_config, &workspace).await,
        TemporaryMcpSession::new("http", &userinfo_config, &workspace).await,
        TemporaryMcpSession::new("http-alias", &alias_config, &workspace).await,
        TemporaryMcpSession::new("http", &config, &alternate_workspace).await,
        TemporaryMcpSession::new_for_scope(
            "http",
            &config,
            &alternate_workspace,
            &alternate_workspace.join("policy-namespace"),
            &McpClientCapabilityPolicy {
                sampling: true,
                ..Default::default()
            },
        )
        .await,
    ];
    let mut unexpected_sessions = Vec::new();
    for session in attempts.into_iter().flatten() {
        unexpected_sessions.push(session);
    }
    for session in &mut unexpected_sessions {
        session.shutdown().await;
    }

    assert!(
        unexpected_sessions.is_empty(),
        "local timeout/stdio/workspace/policy/capability changes must not bypass one remote DELETE domain"
    );
    assert_eq!(
        server_state.next_generation.load(Ordering::Acquire),
        initialize_count_after_first,
        "no replacement initialize may reach the same remote endpoint"
    );

    server_state.release_delete.notify_waiters();
    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborted_one_shot_initialize_quarantines_before_a_second_post() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-one-shot-init-abort");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create one-shot initialize abort workspace");
        let (url, server_state, server_task) = spawn_blocked_initialize_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_first_initialize.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);

        let constructor_config = config.clone();
        let constructor_workspace = workspace.clone();
        let constructor = cleanup.spawn_worker(async move {
            TemporaryMcpSession::new("http", &constructor_config, &constructor_workspace).await
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.initialize_started.notified(),
        )
        .await
        .expect("the first initialize POST should reach the server");
        constructor.abort();
        let join_error = match constructor.await {
            Ok(_) => panic!("constructor should be cancelled"),
            Err(error) => error,
        };
        assert!(join_error.is_cancelled());

        let replacement = TemporaryMcpSession::new("http", &config, &workspace).await;
        let replacement_was_created = replacement.is_ok();
        if let Ok(mut session) = replacement {
            session.shutdown().await;
        }
        assert!(
            !replacement_was_created,
            "a cancelled initialize with an unknown remote outcome must block a second POST"
        );
        assert_eq!(
            server_state.initialize_count.load(Ordering::Acquire),
            1,
            "the second initialize must be rejected before network I/O"
        );

        let first_finished = server_state.first_initialize_finished.notified();
        server_state.release_first_initialize.notify_one();
        tokio::time::timeout(Duration::from_secs(2), first_finished)
            .await
            .expect("the abandoned first initialize should finish remotely");
        assert_eq!(server_state.delete_count.load(Ordering::Acquire), 0);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_after_initialize_install_removes_that_generation_and_blocks_active_reuse() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-init-installed-abort");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create installed initialize-abort workspace");
        let (url, server_state, server_task) =
            spawn_blocked_initialized_notification_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_notification.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let server = config
            .mcp_servers
            .get("http")
            .expect("installed initialize-abort server");
        let cache_key = cache_key("http", server, &workspace, &config)
            .expect("build installed initialize-abort cache key");
        let domain_key =
            http_remote_cleanup_domain_key(server).expect("installed initialize-abort domain");

        let call = cleanup.spawn_worker({
            let config = config.clone();
            let workspace = workspace.clone();
            async move { call_http_server("http", &config, &workspace, "ping", json!({})).await }
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.notification_started.notified(),
        )
        .await
        .expect("notifications/initialized must block after the Session ID was installed");
        let installed = cached_http_session_identity_unchecked(&cache_key)
            .expect("initialize response should already have installed a local generation");
        assert_eq!(installed.session_id, "installed-before-notification");

        call.abort();
        assert!(
            call.await
                .expect_err("initialize caller should be cancelled")
                .is_cancelled()
        );
        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain)
        );
        assert!(cached_http_session_identity_unchecked(&cache_key).is_none());
        {
            let state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            assert!(!state.last_event_ids.contains_key(&cache_key));
            assert!(!state.stream_tasks.contains_key(&cache_key));
        }
        assert_http_descriptor_caches_absent(&cache_key);

        let error = call_http_server("http", &config, &workspace, "ping", json!({}))
            .await
            .expect_err("endpoint quarantine must stop the next ordinary call before initialize");
        assert_eq!(error, HTTP_MCP_CLEANUP_UNCERTAIN_ERROR);
        assert_eq!(server_state.initialize_count.load(Ordering::Acquire), 1);
        assert_eq!(server_state.normal_request_count.load(Ordering::Acquire), 0);
        assert_eq!(server_state.delete_count.load(Ordering::Acquire), 0);

        server_state.release_notification.notify_waiters();
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_http_session_fast_path_obeys_remote_cleanup_quarantine() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-active-quarantine");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create active quarantine workspace");
    let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .get("http")
        .expect("active quarantine server");
    let cache_key =
        cache_key("http", server, &workspace, &config).expect("build active quarantine cache key");
    let domain_key = http_remote_cleanup_domain_key(server).expect("active quarantine domain");

    call_http_server("http", &config, &workspace, "ping", json!({}))
        .await
        .expect("warm active quarantine Session");
    assert!(cached_http_session_identity_unchecked(&cache_key).is_some());
    let normal_count = server_state.normal_request_count.load(Ordering::Acquire);
    let initialize_count = server_state.next_generation.load(Ordering::Acquire);
    ensure_http_cleanup_uncertain(&domain_key);

    let error = call_http_server("http", &config, &workspace, "ping", json!({}))
        .await
        .expect_err("an active local Session must not bypass endpoint quarantine");
    assert_eq!(error, HTTP_MCP_CLEANUP_UNCERTAIN_ERROR);
    assert_eq!(
        server_state.normal_request_count.load(Ordering::Acquire),
        normal_count
    );
    assert_eq!(
        server_state.next_generation.load(Ordering::Acquire),
        initialize_count
    );

    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_initialize_owner_cannot_remove_a_replacement_generation() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-init-replacement-drop");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create initialize replacement workspace");
    let (url, _server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .get("http")
        .expect("initialize replacement server");
    let workspace_root =
        resolve_path_checked(".", &workspace).expect("resolve initialize replacement workspace");
    let capabilities = McpClientCapabilityPolicy::default();
    let old_key = "http\ninitialize-owner-old";
    let replacement_key = "http\ninitialize-owner-replacement";

    initialize_http_session(
        "http",
        server,
        old_key,
        &workspace_root,
        &capabilities,
        false,
        2,
    )
    .await
    .expect("install old initialize generation");
    let old_identity = cached_http_session_identity_unchecked(old_key)
        .expect("old initialize generation should be cached");
    let (endpoint_guard, endpoint_scope) = http_remote_cleanup_guard("http", server, old_key)
        .await
        .expect("acquire initialize owner endpoint authority");
    let mut old_owner = HttpOneShotInitializeAttempt::new(&endpoint_scope);
    old_owner.mark_may_have_been_sent();
    old_owner.bind_installed_session(old_key, &old_identity);
    drop(endpoint_guard);

    initialize_http_session(
        "http",
        server,
        replacement_key,
        &workspace_root,
        &capabilities,
        false,
        2,
    )
    .await
    .expect("install same-ID replacement generation");
    let replacement = cached_http_session_identity_unchecked(replacement_key)
        .expect("replacement generation should be cached");
    drop(old_owner);
    assert_eq!(
        cached_http_session_identity_unchecked(replacement_key),
        Some(replacement),
        "the old owner's Drop must not remove a replacement generation"
    );
    assert!(cached_http_session_identity_unchecked(old_key).is_none());

    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_initialized_one_shot_session_quarantines_its_endpoint() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-one-shot-drop-after-handoff");
    let alternate_workspace =
        unique_temp_workspace("lingclaw-http-one-shot-drop-after-handoff-alt");
    cleanup.track_path(workspace.clone());
    cleanup.track_path(alternate_workspace.clone());
    fs::create_dir_all(&workspace).expect("create one-shot Drop workspace");
    fs::create_dir_all(&alternate_workspace).expect("create alternate Drop workspace");
    let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);

    let session = TemporaryMcpSession::new("http", &config, &workspace)
        .await
        .expect("one-shot Session should initialize before Drop");
    let initialize_count = server_state.next_generation.load(Ordering::Acquire);
    drop(session);

    let replacement = TemporaryMcpSession::new("http", &config, &alternate_workspace).await;
    assert!(
        replacement.is_err(),
        "Drop after initialize handoff must synchronously quarantine the endpoint"
    );
    assert_eq!(
        server_state.next_generation.load(Ordering::Acquire),
        initialize_count,
        "Drop must block replacement initialize before network I/O"
    );

    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_inflight_one_shot_request_quarantines_after_handoff() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-one-shot-request-abort");
        let alternate_workspace = unique_temp_workspace("lingclaw-http-one-shot-request-abort-alt");
        cleanup.track_path(workspace.clone());
        cleanup.track_path(alternate_workspace.clone());
        fs::create_dir_all(&workspace).expect("create request-abort workspace");
        fs::create_dir_all(&alternate_workspace).expect("create alternate request-abort workspace");
        let (url, server_state, server_task) =
            spawn_blocked_one_shot_request_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_tool_call.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);

        let mut session = TemporaryMcpSession::new("http", &config, &workspace)
            .await
            .expect("one-shot Session should initialize before request abort");
        let request = cleanup.spawn_worker(async move {
            session
                .request(&workspace, "tools/call", json!({"name": "blocked"}))
                .await
        });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.tool_call_started.notified(),
        )
        .await
        .expect("tools/call should reach the server before abort");
        request.abort();
        assert!(
            request
                .await
                .expect_err("request task should be cancelled")
                .is_cancelled()
        );

        let replacement = TemporaryMcpSession::new("http", &config, &alternate_workspace).await;
        assert!(
            replacement.is_err(),
            "an aborted post-handoff request must quarantine the endpoint"
        );
        assert_eq!(server_state.initialize_count.load(Ordering::Acquire), 1);
        assert_eq!(server_state.delete_count.load(Ordering::Acquire), 0);

        server_state.release_tool_call.notify_waiters();
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panicking_after_one_shot_handoff_quarantines_the_endpoint() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-one-shot-handoff-panic");
        let alternate_workspace = unique_temp_workspace("lingclaw-http-one-shot-handoff-panic-alt");
        cleanup.track_path(workspace.clone());
        cleanup.track_path(alternate_workspace.clone());
        fs::create_dir_all(&workspace).expect("create handoff-panic workspace");
        fs::create_dir_all(&alternate_workspace).expect("create alternate handoff-panic workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        let config = test_config_with_streamable_http_server(url);

        let panic_config = config.clone();
        let panic_workspace = workspace.clone();
        let task = cleanup.spawn_worker(async move {
            let _session = TemporaryMcpSession::new("http", &panic_config, &panic_workspace)
                .await
                .expect("one-shot Session should initialize before panic");
            panic!("intentional panic after one-shot initialize handoff");
        });
        assert!(
            task.await
                .expect_err("post-handoff task should panic")
                .is_panic()
        );
        let initialize_count = server_state.next_generation.load(Ordering::Acquire);

        let replacement = TemporaryMcpSession::new("http", &config, &alternate_workspace).await;
        assert!(replacement.is_err());
        assert_eq!(
            server_state.next_generation.load(Ordering::Acquire),
            initialize_count
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_after_one_shot_request_error_keeps_the_endpoint_quarantined() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-one-shot-request-error");
    let alternate_workspace = unique_temp_workspace("lingclaw-http-one-shot-request-error-alt");
    cleanup.track_path(workspace.clone());
    cleanup.track_path(alternate_workspace.clone());
    fs::create_dir_all(&workspace).expect("create request-error workspace");
    fs::create_dir_all(&alternate_workspace).expect("create alternate request-error workspace");
    let (url, server_state, server_task) =
        spawn_initialize_failure_http_test_server(4, StatusCode::NO_CONTENT).await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);

    let mut session = TemporaryMcpSession::new("http", &config, &workspace)
        .await
        .expect("one-shot Session should initialize before request error");
    let error = session
        .request(&workspace, "tools/call", json!({"name": "fails"}))
        .await
        .expect_err("tools/call should return its transport error");
    assert!(error.contains("500"), "{error}");
    drop(session);

    let replacement = TemporaryMcpSession::new("http", &config, &alternate_workspace).await;
    assert!(replacement.is_err());
    assert_eq!(server_state.initialize_count.load(Ordering::Acquire), 1);
    assert_eq!(server_state.delete_count.load(Ordering::Acquire), 0);

    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_shutdown_before_delete_quarantine_is_installed_fails_closed() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-one-shot-shutdown-pre-delete-abort");
        let alternate_workspace =
            unique_temp_workspace("lingclaw-http-one-shot-shutdown-pre-delete-abort-alt");
        cleanup.track_path(workspace.clone());
        cleanup.track_path(alternate_workspace.clone());
        fs::create_dir_all(&workspace).expect("create pre-DELETE abort workspace");
        fs::create_dir_all(&alternate_workspace)
            .expect("create alternate pre-DELETE abort workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        let config = test_config_with_streamable_http_server(url);

        let session = TemporaryMcpSession::new("http", &config, &workspace)
            .await
            .expect("one-shot Session should initialize before shutdown abort");
        let cache_key = match &session {
            TemporaryMcpSession::Http(session) => session.cache_key.clone(),
            TemporaryMcpSession::Stdio(_) => panic!("expected HTTP one-shot Session"),
        };
        let blocker = http_key_exclusive_guard(&cache_key)
            .await
            .expect("hold per-key cleanup lock");
        let (waiting_tx, waiting_rx) = tokio::sync::oneshot::channel();
        install_http_exclusive_wait_signal(&cache_key, waiting_tx);
        let shutdown = cleanup.spawn_worker(async move {
            let mut session = session;
            session.shutdown().await;
        });
        tokio::time::timeout(Duration::from_secs(2), waiting_rx)
            .await
            .expect("shutdown should poll the blocked per-key lock")
            .expect("shutdown wait signal should arrive");
        shutdown.abort();
        assert!(
            shutdown
                .await
                .expect_err("shutdown task should be cancelled")
                .is_cancelled()
        );
        drop(blocker);

        let replacement = TemporaryMcpSession::new("http", &config, &alternate_workspace).await;
        assert!(
            replacement.is_err(),
            "lifecycle Drop must quarantine when shutdown is cancelled before DELETE begins"
        );
        assert_eq!(server_state.delete_count.load(Ordering::Acquire), 0);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn known_initialize_failures_delete_the_installed_session_before_recovery() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    for mode in [0, 3] {
        let workspace =
            unique_temp_workspace(&format!("lingclaw-http-one-shot-known-init-failure-{mode}"));
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create known initialize failure workspace");
        let (url, server_state, server_task) =
            spawn_initialize_failure_http_test_server(mode, StatusCode::NO_CONTENT).await;
        cleanup.track_task(server_task);
        let config = test_config_with_streamable_http_server(url);

        let result = TemporaryMcpSession::new("http", &config, &workspace).await;
        assert!(result.is_err(), "initialize failure mode {mode} must fail");
        assert_eq!(
            server_state.delete_count.load(Ordering::Acquire),
            1,
            "a known Session ID from failure mode {mode} must be deleted"
        );
        let domain_key = http_remote_cleanup_domain_key(
            config
                .mcp_servers
                .get("http")
                .expect("known failure server config"),
        )
        .expect("known failure cleanup domain");
        assert_eq!(http_cleanup_phase(&domain_key), None);

        server_state.mode.store(1, Ordering::Release);
        let mut recovered = TemporaryMcpSession::new("http", &config, &workspace)
            .await
            .unwrap_or_else(|error| {
                panic!("confirmed cleanup for mode {mode} should recover: {error}")
            });
        recovered.shutdown().await;
        assert_eq!(server_state.delete_count.load(Ordering::Acquire), 2);
        clear_mcp_caches_for_test().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn initialized_notification_failure_with_accepted_delete_stays_quarantined() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-one-shot-initialized-failure");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create initialized notification failure workspace");
    let (url, server_state, server_task) =
        spawn_initialize_failure_http_test_server(2, StatusCode::ACCEPTED).await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);

    let result = TemporaryMcpSession::new("http", &config, &workspace).await;
    assert!(
        result.is_err(),
        "one-shot initialization must not ignore notifications/initialized failure"
    );
    assert_eq!(
        server_state
            .initialized_notification_count
            .load(Ordering::Acquire),
        1
    );
    assert_eq!(server_state.delete_count.load(Ordering::Acquire), 1);
    let replacement = TemporaryMcpSession::new("http", &config, &workspace).await;
    assert!(
        replacement.is_err(),
        "202 cleanup after initialized notification failure must quarantine the endpoint"
    );
    assert_eq!(server_state.initialize_count.load(Ordering::Acquire), 1);

    clear_mcp_caches_for_test().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panicking_one_shot_constructor_retains_initialize_quarantine() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-one-shot-init-panic");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create one-shot initialize panic workspace");
        let (url, server_state, server_task) = spawn_blocked_initialize_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_first_initialize.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);

        let constructor_config = config.clone();
        let constructor_workspace = workspace.clone();
        let constructor_state = server_state.clone();
        let constructor = cleanup.spawn_worker(async move {
            let construction =
                TemporaryMcpSession::new("http", &constructor_config, &constructor_workspace);
            tokio::pin!(construction);
            tokio::select! {
                _ = &mut construction => panic!("constructor unexpectedly completed before panic"),
                _ = constructor_state.initialize_started.notified() => {
                    panic!("intentional constructor panic after initialize reached the server")
                }
            }
        });
        let join_error = constructor
            .await
            .expect_err("constructor task should panic intentionally");
        assert!(join_error.is_panic());

        let replacement = TemporaryMcpSession::new("http", &config, &workspace).await;
        assert!(replacement.is_err());
        assert_eq!(server_state.initialize_count.load(Ordering::Acquire), 1);

        let first_finished = server_state.first_initialize_finished.notified();
        server_state.release_first_initialize.notify_one();
        tokio::time::timeout(Duration::from_secs(2), first_finished)
            .await
            .expect("the panicked constructor's request should finish remotely");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_invalidation_cleans_known_inflight_one_shot_before_recovery() {
    run_panic_safe_mcp_test!(cleanup, {
        for server_only in [false, true] {
            let workspace = unique_temp_workspace(&format!(
                "lingclaw-http-one-shot-init-invalidate-{server_only}"
            ));
            cleanup.track_path(workspace.clone());
            fs::create_dir_all(&workspace).expect("create initialize invalidation workspace");
            let (url, server_state, server_task) =
                spawn_blocked_initialize_http_test_server().await;
            cleanup.track_task(server_task);
            cleanup.track_barrier_release({
                let server_state = server_state.clone();
                move || server_state.release_first_initialize.notify_waiters()
            });
            let config = test_config_with_streamable_http_server(url);

            let constructor_config = config.clone();
            let constructor_workspace = workspace.clone();
            let constructor = cleanup.spawn_worker(async move {
                TemporaryMcpSession::new("http", &constructor_config, &constructor_workspace).await
            });
            tokio::time::timeout(
                Duration::from_secs(2),
                server_state.initialize_started.notified(),
            )
            .await
            .expect("initialize should reach the server before invalidation");

            if server_only {
                clear_cached_runtime_state_for_server("http");
            } else {
                invalidate_runtime_state_without_remote_shutdown().await;
            }
            server_state.release_first_initialize.notify_one();
            let constructor_result = constructor
                .await
                .expect("join invalidated one-shot constructor");
            assert!(constructor_result.is_err());

            assert_eq!(
                server_state.delete_count.load(Ordering::Acquire),
                1,
                "the invalidated response's known remote Session must be deleted"
            );
            let mut replacement = TemporaryMcpSession::new("http", &config, &workspace)
                .await
                .expect("confirmed cleanup may release the endpoint for a fresh initialize");
            assert_eq!(server_state.initialize_count.load(Ordering::Acquire), 2);
            replacement.shutdown().await;
            clear_mcp_caches_for_test().await;
        }
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_runtime_owned_one_shot_delete_keeps_the_remote_domain_quarantined() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-one-shot-runtime-cleanup-cancel");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create runtime cleanup cancellation workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_delete.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);
        let domain_key = http_remote_cleanup_domain_key(
            config
                .mcp_servers
                .get("http")
                .expect("runtime cleanup server config"),
        )
        .expect("runtime cleanup domain");

        let mut session = TemporaryMcpSession::new("http", &config, &workspace)
            .await
            .expect("runtime cleanup Session should initialize");
        let cleanup_caller = cleanup.spawn_worker(async move { session.shutdown().await });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("runtime-owned DELETE should reach the server");
        {
            let state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            let cleanup_task = state
                .cleanup_tasks
                .values()
                .next()
                .expect("runtime cleanup task should be registered");
            cleanup_task._handle.abort();
        }
        cleanup_caller.await.expect("join cleanup caller");

        assert_eq!(
            http_cleanup_phase(&domain_key),
            Some(HttpCleanupPhase::Uncertain)
        );
        let replacement = TemporaryMcpSession::new("http", &config, &workspace).await;
        assert!(replacement.is_err());
        assert_eq!(server_state.next_generation.load(Ordering::Acquire), 3);

        server_state.release_delete.notify_waiters();
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_shot_delete_timeout_quarantines_the_stable_scope_without_unbounded_keys() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-one-shot-timeout");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create one-shot timeout workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_delete.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);

        let mut first = TemporaryMcpSession::new("http", &config, &workspace)
            .await
            .expect("first one-shot session should initialize");
        let cleanup_caller = cleanup.spawn_worker(async move { first.shutdown().await });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("one-shot DELETE should reach the timeout barrier");
        tokio::time::timeout(Duration::from_secs(4), cleanup_caller)
            .await
            .expect("one-shot cleanup should stop waiting at its client deadline")
            .expect("join one-shot cleanup caller");

        let before = {
            let state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            (
                state.controls.keys().cloned().collect::<HashSet<_>>(),
                state.cleanups.keys().cloned().collect::<HashSet<_>>(),
                state.cleanup_tasks.len(),
                state.remote_domains_by_server.clone(),
                state.remote_domain_by_cache_key.clone(),
            )
        };
        for _ in 0..16 {
            let attempt = TemporaryMcpSession::new("http", &config, &workspace).await;
            assert!(
                attempt.is_err(),
                "the same one-shot base scope must remain quarantined"
            );
        }
        let after = {
            let state = http_runtime_state()
                .lock()
                .expect("HTTP runtime state lock");
            (
                state.controls.keys().cloned().collect::<HashSet<_>>(),
                state.cleanups.keys().cloned().collect::<HashSet<_>>(),
                state.cleanup_tasks.len(),
                state.remote_domains_by_server.clone(),
                state.remote_domain_by_cache_key.clone(),
            )
        };
        assert_eq!(
            after, before,
            "retries in one quarantined scope must not allocate random controls or tombstones"
        );

        let independent_workspace = unique_temp_workspace("lingclaw-http-one-shot-independent");
        cleanup.track_path(independent_workspace.clone());
        fs::create_dir_all(&independent_workspace).expect("create independent one-shot workspace");
        let (independent_url, _independent_log, independent_task) =
            spawn_auth_recording_streamable_http_test_server().await;
        cleanup.track_task(independent_task);
        let independent_config = test_config_with_streamable_http_server(independent_url);
        let mut independent =
            TemporaryMcpSession::new("http", &independent_config, &independent_workspace)
                .await
                .expect("an independent one-shot base scope must still initialize");
        independent.shutdown().await;

        let delete_finished = server_state.delete_finished.notified();
        server_state.release_delete.notify_one();
        tokio::time::timeout(Duration::from_secs(2), delete_finished)
            .await
            .expect("the timed-out server-side DELETE should eventually finish");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_one_shot_cleanup_cannot_bypass_its_stable_scope_authority() {
    run_panic_safe_mcp_test!(cleanup, {
        let workspace = unique_temp_workspace("lingclaw-http-one-shot-abort");
        cleanup.track_path(workspace.clone());
        fs::create_dir_all(&workspace).expect("create one-shot abort workspace");
        let (url, server_state, server_task) = spawn_same_id_reuse_http_test_server().await;
        cleanup.track_task(server_task);
        cleanup.track_barrier_release({
            let server_state = server_state.clone();
            move || server_state.release_delete.notify_waiters()
        });
        let config = test_config_with_streamable_http_server(url);

        let mut first = TemporaryMcpSession::new("http", &config, &workspace)
            .await
            .expect("first one-shot session should initialize");
        let cleanup_caller = cleanup.spawn_worker(async move { first.shutdown().await });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("one-shot DELETE should reach the cancellation barrier");
        cleanup_caller.abort();
        assert!(
            cleanup_caller
                .await
                .expect_err("cleanup caller should be cancelled")
                .is_cancelled()
        );

        let replacement = TemporaryMcpSession::new("http", &config, &workspace).await;
        assert!(
            replacement.is_err(),
            "a new random instance key must not bypass the pending stable-scope cleanup"
        );

        let delete_finished = server_state.delete_finished.notified();
        server_state.release_delete.notify_one();
        tokio::time::timeout(Duration::from_secs(2), delete_finished)
            .await
            .expect("the runtime-owned DELETE should finish after caller cancellation");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if http_runtime_state()
                    .lock()
                    .expect("HTTP runtime state lock")
                    .cleanup_tasks
                    .is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("confirmed runtime-owned cleanup should release its stable scope");

        let mut recovered = TemporaryMcpSession::new("http", &config, &workspace)
            .await
            .expect("confirmed cleanup should permit the stable scope to recover");
        let recovered_cleanup = cleanup.spawn_worker(async move { recovered.shutdown().await });
        tokio::time::timeout(
            Duration::from_secs(2),
            server_state.delete_started.notified(),
        )
        .await
        .expect("recovered one-shot DELETE should reach the server");
        server_state.release_delete.notify_one();
        recovered_cleanup
            .await
            .expect("join recovered one-shot cleanup");
    });
}

#[tokio::test]
async fn confirmed_one_shot_cleanup_allows_same_scope_reuse_and_reclaims_controls() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-one-shot-confirmed");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create confirmed one-shot workspace");
    let (url, log, server_task) = spawn_auth_recording_streamable_http_test_server().await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let baseline = http_runtime_state()
        .lock()
        .expect("HTTP runtime state lock")
        .controls
        .len();

    for _ in 0..2 {
        let mut session = TemporaryMcpSession::new("http", &config, &workspace)
            .await
            .expect("confirmed cleanup should allow one-shot scope reuse");
        session.shutdown().await;
    }

    let calls = log.lock().await;
    assert_eq!(
        calls
            .iter()
            .filter(|entry| entry["method"] == "initialize")
            .count(),
        2
    );
    assert_eq!(
        calls
            .iter()
            .filter(|entry| entry["method"] == "DELETE")
            .count(),
        2
    );
    drop(calls);
    {
        let state = http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock");
        assert_eq!(state.controls.len(), baseline);
        assert!(state.sessions.is_empty());
        assert!(state.cleanups.is_empty());
        assert!(state.cleanup_tasks.is_empty());
        assert!(state.remote_domains_by_server.is_empty());
        assert!(state.remote_domain_by_cache_key.is_empty());
    }

    clear_mcp_caches_for_test().await;
}

#[tokio::test]
async fn ordinary_http_controls_reclaim_after_requests_and_cleanup_finish() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-http-control-reclamation");
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create HTTP control reclamation workspace");
    let (url, _log, server_task) = spawn_auth_recording_streamable_http_test_server().await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .get("http")
        .expect("HTTP test server config")
        .clone();
    let workspace_root =
        resolve_path_checked(".", &workspace).expect("resolve control reclamation workspace");
    let baseline = http_runtime_state()
        .lock()
        .expect("HTTP runtime state lock")
        .controls
        .len();

    for index in 0..48 {
        let cache_key = format!("http\nreclaim-{index}");
        http_post_json(
            "http",
            &server,
            &cache_key,
            &workspace_root,
            &McpClientCapabilityPolicy::default(),
            json!({
                "jsonrpc": "2.0",
                "id": 1000 + index,
                "method": "initialize",
                "params": {}
            }),
            None,
            5,
        )
        .await
        .expect("unique HTTP initialize should succeed");
        terminate_http_session("http", &cache_key, &server).await;
    }
    {
        let state = http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock");
        assert_eq!(state.controls.len(), baseline);
        assert!(state.sessions.is_empty());
        assert!(state.last_event_ids.is_empty());
        assert!(state.stream_tasks.is_empty());
        assert!(state.cleanups.is_empty());
        assert!(state.cleanup_tasks.is_empty());
    }

    let cache_key = "http\nin-flight-reclamation";
    let authority = capture_http_request_authority(cache_key, None, true)
        .expect("capture in-flight initialize authority");
    let old_epoch = authority.epoch;
    remove_http_session(cache_key);
    {
        let state = http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock");
        let control = state
            .controls
            .get(cache_key)
            .expect("an in-flight lease must retain the invalidated control");
        assert_ne!(control.epoch.load(Ordering::Acquire), old_epoch);
    }
    drop(authority);
    assert!(
        !http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock")
            .controls
            .contains_key(cache_key),
        "the final in-flight lease should reclaim an otherwise empty tombstone"
    );
    let fresh = capture_http_request_authority(cache_key, None, true)
        .expect("capture fresh authority after reclamation");
    assert_ne!(fresh.epoch, old_epoch);
    drop(fresh);
    assert_eq!(
        http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock")
            .controls
            .len(),
        baseline
    );

    clear_mcp_caches_for_test().await;
}

#[tokio::test]
async fn cached_tool_definitions_do_not_start_server_on_cache_miss() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-cache-miss");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("normal", &log_path);

    let tools = cached_tool_definitions_openai(&config, &workspace);
    let (cached_servers, enabled_servers) = cached_server_counts(&config, &workspace);

    assert!(tools.is_empty());
    assert_eq!(cached_servers, 0);
    assert_eq!(enabled_servers, 1);
    assert_eq!(log_line_count(&log_path, "tools/list"), 0);

    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn plan_only_tools_rediscover_policy_enabled_read_only_tools_after_cache_clear() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-plan-cold-cache");
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("normal", &log_path);
    let discovered = list_tools(&config, &workspace).await;
    let exposed_name = discovered
        .first()
        .expect("mock server should expose a tool")
        .exposed_name
        .clone();
    save_session_policy(
        &workspace,
        &McpSessionPolicy {
            enabled_servers: HashSet::from(["mock".to_string()]),
            enabled_tools: HashSet::from([exposed_name.clone()]),
            ..Default::default()
        },
    )
    .expect("session policy should save");
    clear_mcp_caches_for_test().await;

    let definitions =
        crate::runtime_loop::build_plan_only_tools(&config, Provider::OpenAI, &workspace).await;
    let names = definitions
        .iter()
        .filter_map(|definition| {
            definition
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
        })
        .collect::<Vec<_>>();

    assert!(names.contains(&exposed_name.as_str()));
    assert!(log_line_count(&log_path, "tools/list") >= 2);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn cached_server_counts_for_policy_ignores_servers_without_enabled_tools() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-policy-cache-count");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let mut config = test_config_with_mcp();
    config.mcp_servers.clear();
    for name in ["allowed", "cold"] {
        config.mcp_servers.insert(
            name.to_string(),
            JsonMcpServerConfig {
                transport: Some("streamable-http".to_string()),
                command: String::new(),
                url: Some(format!("http://127.0.0.1:9/{name}")),
                args: Vec::new(),
                env: HashMap::new(),
                headers: HashMap::new(),
                cwd: None,
                enabled: true,
                auth: None,
                timeout_secs: Some(1),
            },
        );
    }

    let exposed_tool = build_exposed_name("allowed", "search");
    let allowed_server = config
        .mcp_servers
        .get("allowed")
        .expect("allowed server should exist");
    let allowed_key =
        cache_key("allowed", allowed_server, &workspace, &config).expect("cache key should build");
    let allowed_authority = capture_http_descriptor_cache_permit(&allowed_key)
        .expect("capture valid HTTP cache authority for the seeded descriptor");
    tool_cache().lock().expect("tool cache lock").insert(
        allowed_key,
        CachedToolDescriptors {
            descriptors: vec![McpToolDescriptor {
                server_name: "allowed".to_string(),
                raw_name: "search".to_string(),
                exposed_name: exposed_tool.clone(),
                description: "Search".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
                annotations: Default::default(),
            }],
            loaded_at: Instant::now(),
            http_authority: Some(allowed_authority.authority.clone()),
        },
    );
    let policy = McpSessionPolicy {
        enabled_servers: HashSet::from(["allowed".to_string(), "cold".to_string()]),
        enabled_tools: HashSet::from([exposed_tool]),
        ..Default::default()
    };

    let (cached_servers, enabled_servers) =
        cached_server_counts_for_policy(&config, &workspace, &policy);

    assert_eq!(cached_servers, 1);
    assert_eq!(
        enabled_servers, 1,
        "servers with no policy-enabled tools should not force an uncached MCP path"
    );

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn policy_caches_are_isolated_by_private_session_home() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;
    let working_directory = unique_temp_workspace("lingclaw-mcp-shared-project");
    let session_home_a = unique_temp_workspace("lingclaw-mcp-session-home-a");
    let session_home_b = unique_temp_workspace("lingclaw-mcp-session-home-b");
    fs::create_dir_all(&working_directory).expect("shared working directory should exist");
    let config = test_config_with_mcp();
    let server = config
        .mcp_servers
        .get("github")
        .expect("github MCP server should exist");
    let exposed_tool = build_exposed_name("github", "search");
    let base_policy = McpSessionPolicy {
        enabled_servers: HashSet::from(["github".to_string()]),
        enabled_tools: HashSet::from([exposed_tool.clone()]),
        ..Default::default()
    };
    save_session_policy(&session_home_a, &base_policy).expect("first policy should save");
    save_session_policy(&session_home_b, &base_policy).expect("second policy should save");
    let policy_a = load_session_policy(&session_home_a);
    let policy_b = load_session_policy(&session_home_b);

    let key_a = cache_key_for_policy("github", server, &working_directory, &config, &policy_a)
        .expect("first scoped cache key should build");
    let key_b = cache_key_for_policy("github", server, &working_directory, &config, &policy_b)
        .expect("second scoped cache key should build");
    assert_ne!(key_a, key_b);

    tool_cache().lock().expect("tool cache lock").insert(
        key_a,
        CachedToolDescriptors {
            descriptors: vec![McpToolDescriptor {
                server_name: "github".to_string(),
                raw_name: "search".to_string(),
                exposed_name: exposed_tool,
                description: "Search".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
                annotations: Default::default(),
            }],
            loaded_at: Instant::now(),
            http_authority: None,
        },
    );

    assert_eq!(
        cached_list_tools_for_policy(&config, &working_directory, &policy_a).len(),
        1
    );
    assert!(cached_list_tools_for_policy(&config, &working_directory, &policy_b).is_empty());

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(working_directory);
    let _ = fs::remove_dir_all(session_home_a);
    let _ = fs::remove_dir_all(session_home_b);
}

#[tokio::test]
async fn streamable_http_tools_list_uses_session_header_and_sse_response() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-streamable-http");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let config = test_config_with_streamable_http_server(url);

    let tools = list_server_tools_uncached("http", &config, &workspace)
        .await
        .expect("streamable HTTP tools/list should succeed");

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].raw_name, "search");
    assert_eq!(tools[0].annotations.read_only_hint, Some(true));
    assert_eq!(tools[0].annotations.destructive_hint, Some(false));
    let calls = log.lock().await.clone();
    assert!(calls.iter().any(|call| call["method"] == "initialize"));
    assert!(
        calls
            .iter()
            .any(|call| { call["method"] == "tools/list" && call["sessionId"] == "test-session" })
    );
    assert!(
        http_session_cache()
            .lock()
            .expect("HTTP session cache lock")
            .is_empty(),
        "uncached HTTP probes should terminate their temporary session"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn stdio_server_env_expands_env_placeholders() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-env-placeholder");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let log_path = workspace.join("mock.log");
    let mut config = test_config_with_mock_server("normal", &log_path);
    config
        .mcp_servers
        .get_mut("mock")
        .expect("mock server should exist")
        .env
        .insert("LINGCLAW_MCP_ENV_CHECK".to_string(), "${PATH}".to_string());

    let _ = list_server_tools_uncached("mock", &config, &workspace)
        .await
        .expect("stdio MCP server should start");

    let log = fs::read_to_string(&log_path).expect("log should read");
    assert!(
        log.contains("env:LINGCLAW_MCP_ENV_CHECK="),
        "mock server should log the expanded env value"
    );
    assert!(
        !log.contains("env:LINGCLAW_MCP_ENV_CHECK=${PATH}"),
        "stdio env placeholders should be expanded before spawning the MCP process"
    );

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn env_placeholder_expands_inside_header_values() {
    let path = std::env::var("PATH").expect("PATH should exist for MCP placeholder test");

    assert_eq!(
        resolve_env_placeholder("Bearer ${PATH}"),
        format!("Bearer {path}")
    );
}

#[tokio::test]
async fn streamable_http_request_uses_configured_timeout() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, handle) = spawn_hanging_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-http-timeout");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let mut config = test_config_with_streamable_http_server(url);
    config
        .mcp_servers
        .get_mut("http")
        .expect("HTTP server should exist")
        .timeout_secs = Some(1);

    let started = Instant::now();
    let error = list_server_tools_uncached("http", &config, &workspace)
        .await
        .expect_err("hanging HTTP MCP server should time out");

    assert!(error.contains("timed out after 1s"));
    assert!(started.elapsed() < Duration::from_secs(5));

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn oauth_start_uses_configured_timeout_for_metadata_probe() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, handle) = spawn_hanging_streamable_http_test_server().await;
    let server = JsonMcpServerConfig {
        transport: Some("streamable-http".to_string()),
        command: String::new(),
        url: Some(url),
        args: Vec::new(),
        env: HashMap::new(),
        headers: HashMap::new(),
        cwd: None,
        enabled: true,
        auth: Some(JsonMcpAuthConfig {
            client_id: Some("configured-client".to_string()),
            client_secret: None,
            scopes: Vec::new(),
        }),
        timeout_secs: Some(1),
    };

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        start_oauth_authorization("http", &server, DEFAULT_PORT, MCP_DEFAULT_HTTP_TIMEOUT_SECS),
    )
    .await
    .expect("OAuth metadata discovery should be bounded by timeoutSecs");

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(5));

    handle.abort();
    clear_mcp_caches_for_test().await;
}

#[tokio::test]
async fn oauth_start_uses_default_timeout_for_metadata_probe() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, handle) = spawn_hanging_streamable_http_test_server().await;
    let server = JsonMcpServerConfig {
        transport: Some("streamable-http".to_string()),
        command: String::new(),
        url: Some(url),
        args: Vec::new(),
        env: HashMap::new(),
        headers: HashMap::new(),
        cwd: None,
        enabled: true,
        auth: Some(JsonMcpAuthConfig {
            client_id: Some("configured-client".to_string()),
            client_secret: None,
            scopes: Vec::new(),
        }),
        timeout_secs: None,
    };

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        start_oauth_authorization("http", &server, DEFAULT_PORT, 1),
    )
    .await
    .expect("OAuth metadata discovery should be bounded by default timeout");

    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(5));

    handle.abort();
    clear_mcp_caches_for_test().await;
}

#[tokio::test]
async fn streamable_http_cancel_and_delete_requests_use_bearer_token() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_auth_recording_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-http-auth-control");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));
    save_auth_state(&McpAuthState {
        servers: HashMap::from([(
            "http".to_string(),
            McpServerAuthState {
                access_token: Some("stored-token".to_string()),
                resource: Some(url.clone()),
                ..Default::default()
            },
        )]),
    })
    .expect("auth state should save");

    let mut config = test_config_with_streamable_http_server(url);
    config
        .mcp_servers
        .get_mut("http")
        .expect("HTTP server should exist")
        .timeout_secs = Some(1);

    let tools = list_server_tools_uncached("http", &config, &workspace)
        .await
        .expect("tools/list should succeed");
    let tool_name = tools
        .first()
        .expect("test server should expose a tool")
        .exposed_name
        .clone();
    let policy = McpSessionPolicy {
        enabled_servers: HashSet::from(["http".to_string()]),
        enabled_tools: HashSet::from([tool_name.clone()]),
        ..Default::default()
    };
    let outcome = execute_tool_for_policy(&tool_name, "{}", &config, &workspace, false, &policy)
        .await
        .expect("MCP tool should produce an outcome");

    assert!(outcome.is_error);
    let calls = log.lock().await.clone();
    assert!(
        calls.iter().any(|call| {
            call["method"] == "notifications/cancelled"
                && call["authorization"] == "Bearer stored-token"
        }),
        "timeout cancellation should include the OAuth bearer token: {calls:?}"
    );
    assert!(
        calls.iter().any(|call| {
            call["method"] == "DELETE" && call["authorization"] == "Bearer stored-token"
        }),
        "one-shot HTTP session DELETE should include the OAuth bearer token: {calls:?}"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn oauth_bearer_token_without_resource_binding_is_accepted() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_auth_recording_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-oauth-no-resource");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));
    save_auth_state(&McpAuthState {
        servers: HashMap::from([(
            "http".to_string(),
            McpServerAuthState {
                access_token: Some("stored-token".to_string()),
                resource: None,
                ..Default::default()
            },
        )]),
    })
    .expect("auth state should save");

    let config = test_config_with_streamable_http_server(url);
    let tools = list_server_tools_uncached("http", &config, &workspace)
        .await
        .expect("token without resource metadata should remain usable");

    assert_eq!(tools.len(), 1);
    let calls = log.lock().await.clone();
    assert!(
        calls.iter().any(|call| {
            call["method"] == "tools/list" && call["authorization"] == "Bearer stored-token"
        }),
        "MCP requests should use valid tokens that have no protected-resource binding: {calls:?}"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn test_mcp_server_uses_requested_server_name_for_oauth_state() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_auth_recording_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-test-oauth-server-name");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));
    save_auth_state(&McpAuthState {
        servers: HashMap::from([(
            "remote".to_string(),
            McpServerAuthState {
                access_token: Some("stored-token".to_string()),
                resource: None,
                ..Default::default()
            },
        )]),
    })
    .expect("auth state should save");

    let mut config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .remove("http")
        .expect("HTTP server should exist");
    let tool_count = test_mcp_server("remote", &server, &workspace, Duration::from_secs(5))
        .await
        .expect("test path should reuse auth state for the requested server");

    assert_eq!(tool_count, 1);
    let calls = log.lock().await.clone();
    assert!(
        calls.iter().any(|call| {
            call["method"] == "tools/list" && call["authorization"] == "Bearer stored-token"
        }),
        "MCP test requests should use the stored OAuth token for the requested server: {calls:?}"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn auth_state_usable_for_server_rejects_resource_mismatch() {
    let config = test_config_with_streamable_http_server("https://current.example/mcp".to_string());
    let server = config
        .mcp_servers
        .get("http")
        .expect("HTTP server should exist");

    assert!(!auth_state_usable_for_server(
        "http",
        server,
        &McpServerAuthState {
            access_token: Some("stored-token".to_string()),
            resource: Some("https://previous.example/mcp".to_string()),
            ..Default::default()
        }
    ));
    assert!(auth_state_usable_for_server(
        "http",
        server,
        &McpServerAuthState {
            access_token: Some("stored-token".to_string()),
            resource: None,
            ..Default::default()
        }
    ));
    assert!(!auth_state_usable_for_server(
        "http",
        server,
        &McpServerAuthState {
            access_token: Some("stored-token".to_string()),
            expires_at: Some(now_unix_secs().saturating_sub(10)),
            resource: None,
            ..Default::default()
        }
    ));
    assert!(auth_state_usable_for_server(
        "http",
        server,
        &McpServerAuthState {
            access_token: Some("stored-token".to_string()),
            refresh_token: Some("refresh-token".to_string()),
            expires_at: Some(now_unix_secs().saturating_sub(10)),
            client_id: Some("client-id".to_string()),
            token_endpoint: Some("https://auth.example/token".to_string()),
            resource: None,
            ..Default::default()
        }
    ));
}

#[tokio::test]
async fn oauth_bearer_token_is_not_used_after_server_url_changes() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_auth_recording_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-oauth-resource-mismatch");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));
    save_auth_state(&McpAuthState {
        servers: HashMap::from([(
            "http".to_string(),
            McpServerAuthState {
                access_token: Some("stored-token".to_string()),
                resource: Some("https://previous.example/mcp".to_string()),
                ..Default::default()
            },
        )]),
    })
    .expect("auth state should save");

    let config = test_config_with_streamable_http_server(url);
    let error = list_server_tools_uncached("http", &config, &workspace)
        .await
        .expect_err("resource mismatch should require OAuth reconnect");

    assert!(error.contains("different resource"));
    assert!(
        log.lock().await.is_empty(),
        "mismatched OAuth tokens must not be sent to the reconfigured endpoint"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn start_oauth_authorization_preserves_existing_token_until_callback_succeeds() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (base, handle) = spawn_oauth_metadata_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-oauth-start-preserve-token");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));
    save_auth_state(&McpAuthState {
        servers: HashMap::from([(
            "http".to_string(),
            McpServerAuthState {
                access_token: Some("existing-access-token".to_string()),
                refresh_token: Some("existing-refresh-token".to_string()),
                expires_at: Some(now_unix_secs().saturating_add(3600)),
                scopes: vec!["old-scope".to_string()],
                client_id: Some("old-client".to_string()),
                client_secret: Some("old-secret".to_string()),
                resource: Some(format!("{base}mcp")),
                token_endpoint: Some(format!("{base}old-token")),
                ..Default::default()
            },
        )]),
    })
    .expect("auth state should save");

    let server = JsonMcpServerConfig {
        transport: Some("streamable-http".to_string()),
        command: String::new(),
        url: Some(format!("{base}mcp")),
        args: Vec::new(),
        env: HashMap::new(),
        headers: HashMap::new(),
        cwd: None,
        enabled: true,
        auth: Some(JsonMcpAuthConfig {
            client_id: Some("configured-client".to_string()),
            client_secret: None,
            scopes: vec!["read".to_string()],
        }),
        timeout_secs: Some(5),
    };

    let started =
        start_oauth_authorization("http", &server, DEFAULT_PORT, MCP_DEFAULT_HTTP_TIMEOUT_SECS)
            .await
            .expect("OAuth start should succeed");
    assert!(started.authorization_url.contains("configured-client"));

    let saved = load_auth_state();
    let state = saved.servers.get("http").expect("server auth should exist");
    assert_eq!(state.access_token.as_deref(), Some("existing-access-token"));
    assert_eq!(
        state.refresh_token.as_deref(),
        Some("existing-refresh-token")
    );
    assert_eq!(state.client_id.as_deref(), Some("old-client"));
    assert_eq!(state.client_secret.as_deref(), Some("old-secret"));
    assert_eq!(state.scopes, vec!["old-scope"]);
    assert_eq!(
        state.token_endpoint.as_deref(),
        Some(format!("{base}old-token").as_str())
    );
    let pending = state.pending.as_ref().expect("OAuth should be pending");
    assert_eq!(pending.client_id, "configured-client");
    assert_eq!(pending.scopes, vec!["read"]);

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn start_oauth_authorization_preserves_unbound_existing_token_until_callback_succeeds() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (base, handle) = spawn_oauth_metadata_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-oauth-start-preserve-unbound-token");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));
    save_auth_state(&McpAuthState {
        servers: HashMap::from([(
            "http".to_string(),
            McpServerAuthState {
                access_token: Some("existing-access-token".to_string()),
                refresh_token: Some("existing-refresh-token".to_string()),
                expires_at: Some(now_unix_secs().saturating_add(3600)),
                scopes: vec!["old-scope".to_string()],
                client_id: Some("old-client".to_string()),
                token_endpoint: Some(format!("{base}old-token")),
                resource: None,
                ..Default::default()
            },
        )]),
    })
    .expect("auth state should save");

    let server = JsonMcpServerConfig {
        transport: Some("streamable-http".to_string()),
        command: String::new(),
        url: Some(format!("{base}mcp")),
        args: Vec::new(),
        env: HashMap::new(),
        headers: HashMap::new(),
        cwd: None,
        enabled: true,
        auth: Some(JsonMcpAuthConfig {
            client_id: Some("configured-client".to_string()),
            client_secret: None,
            scopes: vec!["read".to_string()],
        }),
        timeout_secs: Some(5),
    };

    start_oauth_authorization("http", &server, DEFAULT_PORT, MCP_DEFAULT_HTTP_TIMEOUT_SECS)
        .await
        .expect("OAuth start should succeed");

    let saved = load_auth_state();
    let state = saved.servers.get("http").expect("server auth should exist");
    assert_eq!(state.access_token.as_deref(), Some("existing-access-token"));
    assert_eq!(
        state.refresh_token.as_deref(),
        Some("existing-refresh-token")
    );
    assert_eq!(state.client_id.as_deref(), Some("old-client"));
    assert_eq!(state.scopes, vec!["old-scope"]);
    assert_eq!(
        state.token_endpoint.as_deref(),
        Some(format!("{base}old-token").as_str())
    );
    assert_eq!(state.resource, None);
    let pending = state.pending.as_ref().expect("OAuth should be pending");
    assert_eq!(pending.client_id, "configured-client");
    assert_eq!(
        pending.resource.as_deref(),
        Some(format!("{base}mcp").as_str())
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn start_oauth_authorization_discovers_path_based_authorization_metadata() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (base, handle) = spawn_oauth_path_issuer_metadata_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-oauth-path-issuer");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));

    let server = JsonMcpServerConfig {
        transport: Some("streamable-http".to_string()),
        command: String::new(),
        url: Some(format!("{base}mcp")),
        args: Vec::new(),
        env: HashMap::new(),
        headers: HashMap::new(),
        cwd: None,
        enabled: true,
        auth: Some(JsonMcpAuthConfig {
            client_id: Some("configured-client".to_string()),
            client_secret: None,
            scopes: vec!["read".to_string()],
        }),
        timeout_secs: Some(5),
    };

    let started =
        start_oauth_authorization("http", &server, DEFAULT_PORT, MCP_DEFAULT_HTTP_TIMEOUT_SECS)
            .await
            .expect("OAuth start should discover path-based issuer metadata");

    assert!(
        started
            .authorization_url
            .starts_with(&format!("{base}tenant/authorize")),
        "authorization URL should come from path-based metadata: {}",
        started.authorization_url
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn start_oauth_authorization_discovers_path_based_oidc_metadata() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (base, handle) = spawn_oauth_path_issuer_oidc_metadata_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-oauth-path-issuer-oidc");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));

    let server = JsonMcpServerConfig {
        transport: Some("streamable-http".to_string()),
        command: String::new(),
        url: Some(format!("{base}mcp")),
        args: Vec::new(),
        env: HashMap::new(),
        headers: HashMap::new(),
        cwd: None,
        enabled: true,
        auth: Some(JsonMcpAuthConfig {
            client_id: Some("configured-client".to_string()),
            client_secret: None,
            scopes: vec!["read".to_string()],
        }),
        timeout_secs: Some(5),
    };

    let started =
        start_oauth_authorization("http", &server, DEFAULT_PORT, MCP_DEFAULT_HTTP_TIMEOUT_SECS)
            .await
            .expect("OAuth start should discover path-based OIDC metadata");

    assert!(
        started
            .authorization_url
            .starts_with(&format!("{base}tenant/authorize")),
        "authorization URL should come from path-based OIDC metadata: {}",
        started.authorization_url
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn start_oauth_authorization_clears_existing_token_for_new_resource() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (base, handle) = spawn_oauth_metadata_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-oauth-start-new-resource");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));
    save_auth_state(&McpAuthState {
        servers: HashMap::from([(
            "http".to_string(),
            McpServerAuthState {
                access_token: Some("old-access-token".to_string()),
                refresh_token: Some("old-refresh-token".to_string()),
                expires_at: Some(now_unix_secs().saturating_add(3600)),
                resource: Some("https://previous.example/mcp".to_string()),
                token_endpoint: Some("https://previous.example/token".to_string()),
                ..Default::default()
            },
        )]),
    })
    .expect("auth state should save");

    let server = JsonMcpServerConfig {
        transport: Some("streamable-http".to_string()),
        command: String::new(),
        url: Some(format!("{base}mcp")),
        args: Vec::new(),
        env: HashMap::new(),
        headers: HashMap::new(),
        cwd: None,
        enabled: true,
        auth: Some(JsonMcpAuthConfig {
            client_id: Some("configured-client".to_string()),
            client_secret: None,
            scopes: vec!["read".to_string()],
        }),
        timeout_secs: Some(5),
    };

    start_oauth_authorization("http", &server, DEFAULT_PORT, MCP_DEFAULT_HTTP_TIMEOUT_SECS)
        .await
        .expect("OAuth start should succeed");

    let saved = load_auth_state();
    let state = saved.servers.get("http").expect("server auth should exist");
    assert_eq!(state.access_token, None);
    assert_eq!(state.refresh_token, None);
    assert_eq!(state.expires_at, None);
    assert_eq!(
        state.resource.as_deref(),
        Some(format!("{base}mcp").as_str())
    );
    assert!(state.pending.is_some());

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn complete_oauth_authorization_clears_cached_runtime_state() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (base, _log, handle) = spawn_oauth_token_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-oauth-complete-clear-cache");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));

    let cache_key = "http\nold-token-cache".to_string();
    tool_cache().lock().expect("tool cache lock").insert(
        cache_key.clone(),
        CachedToolDescriptors {
            descriptors: vec![McpToolDescriptor {
                server_name: "http".to_string(),
                raw_name: "old".to_string(),
                exposed_name: "mcp__http__old__00000000".to_string(),
                description: "old".to_string(),
                input_schema: json!({}),
                annotations: Default::default(),
            }],
            loaded_at: Instant::now(),
            http_authority: None,
        },
    );
    let workspace_root =
        resolve_path_checked(".", &workspace).expect("resolve cached OAuth workspace");
    set_http_session_id(
        &cache_key,
        Some("old-session".to_string()),
        &workspace_root,
        true,
    )
    .expect("install cached OAuth HTTP session");
    save_auth_state(&McpAuthState {
        servers: HashMap::from([(
            "http".to_string(),
            McpServerAuthState {
                access_token: Some("old-token".to_string()),
                refresh_token: Some("old-refresh".to_string()),
                pending: Some(McpPendingOAuthState {
                    state: "callback-state".to_string(),
                    code_verifier: "verifier".to_string(),
                    redirect_uri: "http://127.0.0.1:18989/api/mcp/auth/callback?server=http"
                        .to_string(),
                    token_endpoint: format!("{base}token"),
                    client_id: "client-id".to_string(),
                    client_secret: None,
                    scopes: vec!["read".to_string()],
                    resource: Some("https://mcp.example/mcp".to_string()),
                }),
                ..Default::default()
            },
        )]),
    })
    .expect("auth state should save");

    let completed = complete_oauth_authorization(
        "http",
        "auth-code",
        "callback-state",
        MCP_DEFAULT_HTTP_TIMEOUT_SECS,
    )
    .await
    .expect("OAuth callback should complete");

    assert_eq!(completed.access_token.as_deref(), Some("new-access-token"));
    assert!(
        !tool_cache()
            .lock()
            .expect("tool cache lock")
            .contains_key(&cache_key),
        "OAuth completion should clear descriptor cache for the server"
    );
    assert!(
        !http_session_cache()
            .lock()
            .expect("HTTP session cache lock")
            .contains_key(&cache_key),
        "OAuth completion should clear cached HTTP sessions for the server"
    );
    assert!(
        !http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock")
            .controls
            .contains_key(&cache_key),
        "OAuth completion should reclaim an idle per-key control"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn start_oauth_authorization_encodes_callback_server_name() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (base, handle) = spawn_oauth_metadata_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-oauth-encoded-callback");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));

    let server = JsonMcpServerConfig {
        transport: Some("streamable-http".to_string()),
        command: String::new(),
        url: Some(format!("{base}mcp")),
        args: Vec::new(),
        env: HashMap::new(),
        headers: HashMap::new(),
        cwd: None,
        enabled: true,
        auth: Some(JsonMcpAuthConfig {
            client_id: Some("configured-client".to_string()),
            client_secret: None,
            scopes: vec!["read".to_string()],
        }),
        timeout_secs: Some(5),
    };
    let server_name = "remote & weird#1";

    let started = start_oauth_authorization(
        server_name,
        &server,
        DEFAULT_PORT,
        MCP_DEFAULT_HTTP_TIMEOUT_SECS,
    )
    .await
    .expect("OAuth start should succeed");
    let callback_url =
        reqwest::Url::parse(&started.redirect_uri).expect("callback URI should parse");
    assert_eq!(
        callback_url
            .query_pairs()
            .find(|(key, _)| key == "server")
            .map(|(_, value)| value.into_owned())
            .as_deref(),
        Some(server_name)
    );
    let authorization_url =
        reqwest::Url::parse(&started.authorization_url).expect("authorization URL should parse");
    assert_eq!(
        authorization_url
            .query_pairs()
            .find(|(key, _)| key == "redirect_uri")
            .map(|(_, value)| value.into_owned())
            .as_deref(),
        Some(started.redirect_uri.as_str())
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn streamable_http_sse_response_timeout_sends_cancel_and_clears_session() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_timeout_sse_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-http-sse-timeout-cancel");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let mut config = test_config_with_streamable_http_server(url);
    config
        .mcp_servers
        .get_mut("http")
        .expect("HTTP server should exist")
        .timeout_secs = Some(1);

    let tools = list_server_tools_uncached("http", &config, &workspace)
        .await
        .expect("tools/list should succeed");
    let tool_name = tools
        .first()
        .expect("test server should expose a tool")
        .exposed_name
        .clone();
    let policy = McpSessionPolicy {
        enabled_servers: HashSet::from(["http".to_string()]),
        enabled_tools: HashSet::from([tool_name.clone()]),
        ..Default::default()
    };
    let outcome = execute_tool_for_policy(&tool_name, "{}", &config, &workspace, false, &policy)
        .await
        .expect("MCP tool should produce an outcome");

    assert!(outcome.is_error);
    assert!(outcome.output.contains("SSE response timed out"));
    let calls = log.lock().await.clone();
    assert!(
        calls.iter().any(|call| {
            call["method"] == "notifications/cancelled"
                && call["sessionId"] == "timeout-sse-session"
        }),
        "SSE response timeout should notify cancellation with the active session id: {calls:?}"
    );
    assert!(
        http_session_cache()
            .lock()
            .expect("HTTP session cache lock")
            .is_empty(),
        "timed-out SSE responses should clear the cached HTTP session"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn streamable_http_returns_from_open_sse_after_matching_response() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_open_sse_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-http-open-sse");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    save_session_policy(
        &workspace,
        &McpSessionPolicy {
            enabled_servers: HashSet::from(["http".to_string()]),
            client_capabilities: McpClientCapabilityPolicy {
                roots: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .expect("policy should save");
    let mut config = test_config_with_streamable_http_server(url);
    config
        .mcp_servers
        .get_mut("http")
        .expect("HTTP server should exist")
        .timeout_secs = Some(5);

    let tools = list_server_tools("http", &config, &workspace)
        .await
        .expect("open SSE stream should return after the matching response event");

    assert_eq!(tools.len(), 1);
    let calls = log.lock().await.clone();
    assert!(
        calls.iter().any(|call| {
            call["method"] == "initialize"
                && call["payload"]["params"]["capabilities"]
                    .get("roots")
                    .is_none()
        }),
        "Streamable HTTP must not advertise local roots capabilities: {calls:?}"
    );
    assert!(
        calls.iter().any(|call| {
            call["payload"]["id"] == 99
                && call["payload"]["error"]["code"] == -32601
                && call["payload"].get("result").is_none()
        }),
        "an unsolicited HTTP roots/list request must fail closed without a root URI: {calls:?}"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn http_roots_are_disabled_across_the_validate_before_send_window() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let fixture = unique_temp_workspace("lingclaw-http-roots-send-barrier");
    cleanup.track_path(fixture.clone());
    let workspace = fixture.join("workspace");
    let moved = fixture.join("workspace-original");
    let outside = fixture.join("outside");
    fs::create_dir_all(&workspace).expect("create HTTP roots workspace");
    fs::create_dir_all(&outside).expect("create HTTP roots outside directory");
    fs::write(workspace.join("identity.txt"), "inside").expect("seed HTTP inside identity");
    fs::write(outside.join("identity.txt"), "outside").expect("seed HTTP outside identity");
    let (url, log, server_task) = spawn_streamable_http_test_server().await;
    cleanup.track_task(server_task);
    let config = test_config_with_streamable_http_server(url);
    let server = config
        .mcp_servers
        .get("http")
        .expect("HTTP roots server config");
    let workspace_root =
        resolve_path_checked(".", &workspace).expect("resolve HTTP roots workspace");
    let requested_capabilities = McpClientCapabilityPolicy {
        roots: true,
        ..Default::default()
    };
    assert!(requested_capabilities.roots);
    assert!(!effective_http_client_capabilities(&requested_capabilities).roots);
    let mut replacement_installed = false;
    #[cfg(windows)]
    let mut move_was_blocked = false;

    handle_http_server_message_async_with_before_send_hook(
        &json!({"jsonrpc": "2.0", "id": 81, "method": "roots/list"}),
        "http-roots-send-barrier",
        "http",
        server,
        &workspace_root,
        &requested_capabilities,
        None,
        5,
        &mut || match fs::rename(&workspace, &moved) {
            Ok(()) => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(&outside, &workspace)
                    .expect("install HTTP outside symlink at send barrier");
                #[cfg(windows)]
                {
                    let output = StdCommand::new("cmd.exe")
                        .arg("/c")
                        .arg("mklink")
                        .arg("/J")
                        .arg(&workspace)
                        .arg(&outside)
                        .output()
                        .expect("run HTTP roots barrier junction command");
                    assert!(
                        output.status.success(),
                        "HTTP roots barrier junction should install: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                replacement_installed = true;
            }
            Err(error) => {
                #[cfg(windows)]
                {
                    assert!(
                        error.kind() == std::io::ErrorKind::PermissionDenied
                            || error.raw_os_error() == Some(32),
                        "unexpected Windows HTTP roots lock error: {error}"
                    );
                    move_was_blocked = true;
                }
                #[cfg(not(windows))]
                panic!("move HTTP roots workspace at send barrier: {error}");
            }
        },
    )
    .await
    .expect("HTTP roots rejection should be delivered safely");

    let calls = log.lock().await.clone();
    let response = calls
        .iter()
        .find(|call| call["payload"]["id"] == 81)
        .expect("HTTP server should receive the roots/list rejection");
    assert_eq!(response["payload"]["error"]["code"], -32601);
    assert!(response["payload"].get("result").is_none());
    let serialized = serde_json::to_string(response).expect("serialize HTTP roots response");
    assert!(!serialized.contains("file://"));
    assert!(!serialized.contains(&outside.to_string_lossy().replace('\\', "/")));
    #[cfg(unix)]
    assert!(
        replacement_installed,
        "Unix send barrier must replace the root"
    );
    #[cfg(windows)]
    assert!(
        replacement_installed || move_was_blocked,
        "Windows send barrier must either retain the root lock or reject roots"
    );

    if replacement_installed {
        #[cfg(unix)]
        fs::remove_file(&workspace).expect("remove HTTP roots barrier symlink");
        #[cfg(windows)]
        fs::remove_dir(&workspace).expect("remove HTTP roots barrier junction");
    }
    drop(workspace_root);
    if moved.exists() {
        fs::rename(&moved, &workspace).expect("restore HTTP roots workspace");
    }
    clear_mcp_caches_for_test().await;
}

#[tokio::test]
async fn http_event_stream_task_is_removed_after_exit() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, _log, handle) = spawn_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-http-stream-cleanup");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let config = test_config_with_streamable_http_server(url);

    let tools = list_server_tools("http", &config, &workspace)
        .await
        .expect("tools/list should succeed");
    assert_eq!(tools.len(), 1);

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        http_stream_tasks()
            .lock()
            .expect("HTTP stream task lock")
            .is_empty(),
        "completed GET SSE task should be removed so a later refresh can reconnect"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn cached_http_session_restarts_dropped_event_stream() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-http-stream-restart");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let config = test_config_with_streamable_http_server(url);

    let first = call_http_server("http", &config, &workspace, "tools/list", json!({}))
        .await
        .expect("first tools/list should succeed");
    assert!(first["tools"].as_array().is_some());

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        http_stream_tasks()
            .lock()
            .expect("HTTP stream task lock")
            .is_empty(),
        "short-lived GET SSE task should have cleaned itself up"
    );

    let second = call_http_server("http", &config, &workspace, "tools/list", json!({}))
        .await
        .expect("second tools/list should succeed");
    assert!(second["tools"].as_array().is_some());

    tokio::time::sleep(Duration::from_millis(100)).await;
    let calls = log.lock().await.clone();
    let get_count = calls.iter().filter(|call| call["method"] == "GET").count();
    assert_eq!(
        get_count, 2,
        "cached HTTP session should restart the event stream after it exits"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn streamable_http_requests_use_distinct_jsonrpc_ids() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-http-unique-ids");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let config = test_config_with_streamable_http_server(url);

    let first = call_http_server("http", &config, &workspace, "tools/list", json!({}))
        .await
        .expect("first tools/list should succeed");
    assert!(first["tools"].as_array().is_some());
    let second = call_http_server("http", &config, &workspace, "tools/list", json!({}))
        .await
        .expect("second tools/list should succeed");
    assert!(second["tools"].as_array().is_some());

    let calls = log.lock().await.clone();
    let ids = calls
        .iter()
        .filter(|call| call["method"] == "tools/list")
        .filter_map(|call| call["payload"]["id"].as_u64())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert_ne!(
        ids[0], ids[1],
        "concurrent Streamable-HTTP requests must not reuse JSON-RPC ids"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_streamable_http_cold_calls_share_initialization() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-http-concurrent-cold");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let config = test_config_with_streamable_http_server(url);

    let left = call_http_server("http", &config, &workspace, "tools/list", json!({}));
    let right = call_http_server("http", &config, &workspace, "tools/list", json!({}));
    let (left, right) = tokio::join!(left, right);

    assert!(left.expect("left tools/list should succeed")["tools"].is_array());
    assert!(right.expect("right tools/list should succeed")["tools"].is_array());

    let calls = log.lock().await.clone();
    let initialize_count = calls
        .iter()
        .filter(|call| call["method"] == "initialize")
        .count();
    let list_count = calls
        .iter()
        .filter(|call| call["method"] == "tools/list")
        .count();
    assert_eq!(
        initialize_count, 1,
        "concurrent cold Streamable-HTTP calls should share one initialize"
    );
    assert_eq!(list_count, 2);

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn stale_http_stream_cleanup_preserves_replacement_task() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let cache_key = "http\nstream-race".to_string();
    let replacement = tokio::spawn(async {
        std::future::pending::<()>().await;
    });
    let replacement_task_id = next_http_stream_task_id();
    {
        let mut tasks = http_stream_tasks().lock().expect("HTTP stream task lock");
        tasks.insert(
            cache_key.clone(),
            HttpStreamTaskEntry {
                task_id: replacement_task_id,
                epoch: 2,
                generation: 2,
                handle: replacement,
            },
        );
    }

    drop(HttpStreamTaskCleanup {
        cache_key: cache_key.clone(),
        task_id: replacement_task_id.saturating_sub(1),
        epoch: 1,
        generation: 1,
    });

    let replacement = {
        let mut tasks = http_stream_tasks().lock().expect("HTTP stream task lock");
        let entry = tasks
            .remove(&cache_key)
            .expect("replacement task should remain tracked");
        assert_eq!(entry.task_id, replacement_task_id);
        entry.handle
    };
    replacement.abort();
    clear_mcp_caches_for_test().await;
}

#[tokio::test]
async fn catalog_snapshot_does_not_leave_cached_http_sessions() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, _log, handle) = spawn_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-catalog-no-session");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let config = test_config_with_streamable_http_server(url);

    let snapshot = catalog_snapshot(&config, &workspace).await;

    assert_eq!(snapshot.tools.len(), 1);
    assert!(
        http_session_cache()
            .lock()
            .expect("HTTP session cache lock")
            .is_empty(),
        "catalog discovery must use one-shot sessions and leave no cached HTTP sessions"
    );
    assert!(
        http_stream_tasks()
            .lock()
            .expect("HTTP stream task lock")
            .is_empty(),
        "catalog discovery must not keep GET SSE tasks running"
    );
    let runtime_summary = {
        let state = http_runtime_state()
            .lock()
            .expect("HTTP runtime state lock");
        (
            state
                .controls
                .iter()
                .map(|(key, control)| (key.clone(), control.leases.load(Ordering::Acquire)))
                .collect::<Vec<_>>(),
            state.cleanups.len(),
            state.cleanup_tasks.len(),
        )
    };
    assert!(
        runtime_summary.0.is_empty(),
        "completed one-shot discovery must reclaim its per-key control: {runtime_summary:?}"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn policy_aware_catalog_ignores_policy_files_in_the_working_directory() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let working_directory = unique_temp_workspace("lingclaw-mcp-project-policy-isolation");
    let session_home = unique_temp_workspace("lingclaw-mcp-private-policy-isolation");
    fs::create_dir_all(&working_directory).expect("working directory should exist");
    fs::create_dir_all(&session_home).expect("private Session Home should exist");
    let log_path = working_directory.join("mock.log");
    let config = test_config_with_mock_server("default", &log_path);

    save_session_policy(
        &working_directory,
        &McpSessionPolicy {
            enabled_servers: HashSet::from(["mock".to_string()]),
            client_capabilities: McpClientCapabilityPolicy {
                roots: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .expect("project policy fixture should save");
    let private_policy = McpSessionPolicy {
        cache_namespace: Some(session_home.clone()),
        ..Default::default()
    };

    let snapshot = catalog_snapshot_for_policy(&config, &working_directory, &private_policy).await;

    assert_eq!(snapshot.tools.len(), 1);
    let log = fs::read_to_string(&log_path).expect("mock MCP log should read");
    assert!(
        !log.contains("\"roots\""),
        "an external project policy must not enable private MCP capabilities"
    );

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&working_directory);
    let _ = fs::remove_dir_all(&session_home);
}

#[tokio::test]
async fn catalog_snapshot_lists_resources_and_prompts_when_tools_list_fails() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, handle) = spawn_resources_only_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-catalog-resources-only");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let config = test_config_with_streamable_http_server(url);

    let snapshot = catalog_snapshot(&config, &workspace).await;

    assert!(snapshot.tools.is_empty());
    assert_eq!(snapshot.resources.len(), 1);
    assert_eq!(snapshot.resources[0].uri, "memo://one");
    assert_eq!(snapshot.prompts.len(), 1);
    assert_eq!(snapshot.prompts[0].raw_name, "summarize");
    assert_eq!(snapshot.reports.len(), 1);
    assert_eq!(snapshot.reports[0].resource_count, 1);
    assert_eq!(snapshot.reports[0].prompt_count, 1);
    assert!(
        snapshot.reports[0].error.is_none(),
        "tools/list failure should not hide resource/prompt catalog entries"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn paginated_list_rejects_repeated_cursor() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, _log, handle) = spawn_repeating_cursor_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-repeated-cursor");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let config = test_config_with_streamable_http_server(url);

    let error = list_server_tools_uncached("http", &config, &workspace)
        .await
        .expect_err("repeated cursors should be rejected");

    assert!(error.contains("repeated pagination cursor"));

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn paginated_list_preserves_base_params_when_adding_cursor() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_filtered_cursor_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-filtered-cursor");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let config = test_config_with_streamable_http_server(url);

    let items = list_server_items(
        "http",
        &config,
        &workspace,
        "tools/list",
        "tools",
        json!({"kind": "docs"}),
        true,
    )
    .await
    .expect("filtered paginated tools/list should succeed");

    assert_eq!(items.len(), 2);
    let calls = log.lock().await.clone();
    let list_calls = calls
        .iter()
        .filter(|call| call["method"] == "tools/list")
        .collect::<Vec<_>>();
    assert_eq!(list_calls.len(), 2);
    assert_eq!(list_calls[0]["payload"]["params"]["kind"], "docs");
    assert_eq!(list_calls[1]["payload"]["params"]["kind"], "docs");
    assert_eq!(list_calls[1]["payload"]["params"]["cursor"], "next");

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn uncached_paginated_streamable_http_list_reuses_temporary_session() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, state, handle) = spawn_session_bound_cursor_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-session-bound-cursor");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let config = test_config_with_streamable_http_server(url);

    let items = list_server_items(
        "http",
        &config,
        &workspace,
        "tools/list",
        "tools",
        json!({}),
        true,
    )
    .await
    .expect("uncached paginated tools/list should keep cursor session");

    assert_eq!(items.len(), 2);
    let state = state.lock().await;
    assert_eq!(
        state.init_count, 1,
        "uncached pagination should initialize one temporary MCP session"
    );
    let list_sessions = state
        .log
        .iter()
        .filter(|call| call["method"] == "tools/list")
        .map(|call| call["sessionId"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        list_sessions,
        vec![
            "cursor-session-1".to_string(),
            "cursor-session-1".to_string()
        ]
    );
    drop(state);

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn clearing_server_runtime_state_removes_cached_http_sessions() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, _log, handle) = spawn_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-clear-server-state");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let config = test_config_with_streamable_http_server(url);

    let tools = list_server_tools("http", &config, &workspace)
        .await
        .expect("tools/list should succeed");
    assert_eq!(tools.len(), 1);
    assert!(
        !http_session_cache()
            .lock()
            .expect("HTTP session cache lock")
            .is_empty(),
        "shared HTTP discovery should cache a session before cleanup"
    );

    let server = config
        .mcp_servers
        .get("http")
        .expect("HTTP server should exist");
    terminate_http_sessions_for_server("http", server).await;
    clear_cached_runtime_state_for_server("http");

    assert!(
        http_session_cache()
            .lock()
            .expect("HTTP session cache lock")
            .is_empty(),
        "disconnect cleanup should clear cached HTTP sessions"
    );
    assert!(
        tool_cache().lock().expect("tool cache lock").is_empty(),
        "disconnect cleanup should clear stale descriptors for that server"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn cached_server_counts_for_policy_requires_enabled_tools_in_cache() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-policy-cache-missing-tool");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("normal", &log_path);

    let server = config
        .mcp_servers
        .get("mock")
        .expect("mock server should exist");
    let cache_key = cache_key("mock", server, &workspace, &config).expect("cache key should build");
    let cached_tool = build_exposed_name("mock", "search");
    let newly_enabled_tool = build_exposed_name("mock", "new_search");
    tool_cache().lock().expect("tool cache lock").insert(
        cache_key.clone(),
        CachedToolDescriptors {
            descriptors: vec![McpToolDescriptor {
                server_name: "mock".to_string(),
                raw_name: "search".to_string(),
                exposed_name: cached_tool,
                description: "Search".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
                annotations: Default::default(),
            }],
            loaded_at: Instant::now(),
            http_authority: None,
        },
    );

    let policy = McpSessionPolicy {
        enabled_servers: HashSet::from(["mock".to_string()]),
        enabled_tools: HashSet::from([newly_enabled_tool]),
        ..Default::default()
    };

    let (cached_servers, enabled_servers) =
        cached_server_counts_for_policy(&config, &workspace, &policy);

    assert_eq!(enabled_servers, 1);
    assert_eq!(
        cached_servers, 0,
        "a server cache missing policy-enabled tools must force fresh discovery"
    );
    assert!(
        !tool_cache()
            .lock()
            .expect("tool cache lock")
            .contains_key(&cache_key),
        "incomplete policy cache should be evicted so refresh cannot reuse stale descriptors"
    );
    assert_eq!(log_line_count(&log_path, "tools/list"), 0);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn disabled_mcp_tool_call_does_not_contact_server_for_discovery() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-disabled-tool-no-discovery");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("normal", &log_path);

    let outcome = execute_tool_for_policy(
        "mcp__mock__search__deadbeef",
        "{}",
        &config,
        &workspace,
        false,
        &McpSessionPolicy::default(),
    )
    .await
    .expect("MCP tool names should be handled");

    assert!(outcome.is_error);
    assert!(outcome.output.contains("not enabled"));
    assert_eq!(log_line_count(&log_path, "start"), 0);
    assert_eq!(log_line_count(&log_path, "tools/list"), 0);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn ensure_policy_tools_cached_does_not_contact_servers_when_policy_is_empty() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-policy-cache-empty");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("normal", &log_path);

    ensure_policy_tools_cached(&config, &workspace).await;

    assert_eq!(log_line_count(&log_path, "start"), 0);
    assert_eq!(log_line_count(&log_path, "tools/list"), 0);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn policy_tool_listing_only_contacts_policy_enabled_servers() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-policy-list-filter");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let allowed_log = workspace.join("allowed.log");
    let blocked_log = workspace.join("blocked.log");

    let mut config = test_config_with_mock_server("normal", &allowed_log);
    let allowed_server = config
        .mcp_servers
        .remove("mock")
        .expect("mock server should exist");
    let mut blocked_server = allowed_server.clone();
    blocked_server.env.insert(
        "LINGCLAW_MCP_LOG".to_string(),
        blocked_log.display().to_string(),
    );
    config
        .mcp_servers
        .insert("allowed".to_string(), allowed_server);
    config
        .mcp_servers
        .insert("blocked".to_string(), blocked_server);

    let tool_name = build_exposed_name("allowed", "alpha");
    let policy = McpSessionPolicy {
        enabled_servers: HashSet::from(["allowed".to_string()]),
        enabled_tools: HashSet::from([tool_name.clone()]),
        confirm_mutating_tools: false,
        client_capabilities: Default::default(),
        cache_namespace: None,
    };

    let tools = list_tools_for_policy(&config, &workspace, &policy).await;

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].exposed_name, tool_name);
    assert_eq!(log_line_count(&allowed_log, "tools/list"), 1);
    assert_eq!(log_line_count(&blocked_log, "start"), 0);
    assert_eq!(log_line_count(&blocked_log, "tools/list"), 0);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn policy_tool_execution_does_not_probe_sanitized_colliding_disabled_servers() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-policy-exec-collision");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let allowed_log = workspace.join("allowed.log");
    let blocked_log = workspace.join("blocked.log");

    let mut config = test_config_with_mock_server("normal", &allowed_log);
    let allowed_server = config
        .mcp_servers
        .remove("mock")
        .expect("mock server should exist");
    let mut blocked_server = allowed_server.clone();
    blocked_server.env.insert(
        "LINGCLAW_MCP_LOG".to_string(),
        blocked_log.display().to_string(),
    );
    config
        .mcp_servers
        .insert("github-repo".to_string(), blocked_server);
    config
        .mcp_servers
        .insert("github_repo".to_string(), allowed_server);

    let tool_name = build_exposed_name("github_repo", "alpha");
    let policy = McpSessionPolicy {
        enabled_servers: HashSet::from(["github_repo".to_string()]),
        enabled_tools: HashSet::from([tool_name.clone()]),
        confirm_mutating_tools: false,
        client_capabilities: Default::default(),
        cache_namespace: None,
    };

    let outcome = execute_tool_for_policy(&tool_name, "{}", &config, &workspace, false, &policy)
        .await
        .expect("enabled MCP tool should return an outcome");

    assert!(!outcome.is_error, "unexpected outcome: {}", outcome.output);
    assert_eq!(outcome.output, "ok");
    assert_eq!(log_line_count(&allowed_log, "tools/list"), 1);
    assert_eq!(log_line_count(&allowed_log, "tools/call"), 1);
    assert_eq!(log_line_count(&blocked_log, "start"), 0);
    assert_eq!(log_line_count(&blocked_log, "tools/list"), 0);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn refresh_servers_lists_resources_and_prompts_when_tools_list_fails() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, handle) = spawn_resources_only_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-refresh-resources-only");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let config = test_config_with_streamable_http_server(url);

    let reports = refresh_servers(&config, &workspace)
        .await
        .expect("refresh should succeed for resource/prompt-only MCP servers");

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].server_name, "http");
    assert!(reports[0].tool_names.is_empty());
    assert_eq!(reports[0].resource_count, 1);
    assert_eq!(reports[0].prompt_count, 1);
    assert_eq!(reports[0].error, None);

    let server = config
        .mcp_servers
        .get("http")
        .expect("test server should exist");
    let key = cache_key("http", server, &workspace, &config).expect("cache key should build");
    assert!(
        !tool_cache()
            .lock()
            .expect("tool cache lock")
            .contains_key(&key),
        "tools/list failure must not cache an empty tool list"
    );
    assert!(
        resource_cache()
            .lock()
            .expect("resource cache lock")
            .contains_key(&key),
        "successful resources/list should still cache resources"
    );
    assert!(
        prompt_cache()
            .lock()
            .expect("prompt cache lock")
            .contains_key(&key),
        "successful prompts/list should still cache prompts"
    );
    let (cached_servers, enabled_servers) = cached_server_counts(&config, &workspace);
    assert_eq!(
        (cached_servers, enabled_servers),
        (0, 1),
        "tool cache completeness should still report the MCP server as uncached"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn expired_oauth_access_token_is_refreshed_before_http_use() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_oauth_token_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-oauth-refresh");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    set_auth_file_path_for_test(workspace.join("mcp-auth.json"));

    save_auth_state(&McpAuthState {
        servers: HashMap::from([(
            "http".to_string(),
            McpServerAuthState {
                access_token: Some("old-token".to_string()),
                refresh_token: Some("refresh-token".to_string()),
                expires_at: Some(now_unix_secs().saturating_sub(10)),
                scopes: vec!["read".to_string()],
                client_id: Some("client-id".to_string()),
                client_secret: Some("client-secret".to_string()),
                resource: Some("https://resource.example".to_string()),
                token_endpoint: Some(format!("{url}token")),
                ..Default::default()
            },
        )]),
    })
    .expect("auth state should save");

    let token = bearer_token_for_server("http", MCP_DEFAULT_HTTP_TIMEOUT_SECS)
        .await
        .expect("refresh should succeed")
        .expect("token should exist");

    assert_eq!(token, "new-access-token");
    let saved = load_auth_state();
    let saved_server = saved.servers.get("http").expect("server auth should save");
    assert_eq!(
        saved_server.access_token.as_deref(),
        Some("new-access-token")
    );
    assert_eq!(
        saved_server.refresh_token.as_deref(),
        Some("new-refresh-token")
    );
    assert_eq!(saved_server.scopes, vec!["read", "write"]);

    let calls = log.lock().await.clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["grant_type"], "refresh_token");
    assert_eq!(calls[0]["refresh_token"], "refresh-token");
    assert_eq!(calls[0]["client_id"], "client-id");
    assert_eq!(calls[0]["client_secret"], "client-secret");
    assert_eq!(calls[0]["resource"], "https://resource.example");

    handle.abort();
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn parallelizable_tool_call_cache_miss_does_not_start_server() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-parallelizable-cache-miss");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("normal", &log_path);
    let tool_name = build_exposed_name("mock", "alpha");

    assert!(!crate::tools::is_parallelizable_tool_call(
        &tool_name, &config, &workspace
    ));
    assert_eq!(log_line_count(&log_path, "start"), 0);
    assert_eq!(log_line_count(&log_path, "tools/list"), 0);

    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn stdio_initialize_advertises_roots_only_when_session_policy_enables_it() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-roots-capability");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("default", &log_path);

    let _ = list_server_tools_uncached("mock", &config, &workspace)
        .await
        .expect("tools/list should succeed");
    let first_log = fs::read_to_string(&log_path).expect("log should read");
    assert!(
        !first_log.contains("\"roots\""),
        "roots must not be advertised by default"
    );

    save_session_policy(
        &workspace,
        &McpSessionPolicy {
            client_capabilities: McpClientCapabilityPolicy {
                roots: true,
                sampling: true,
                elicitation: true,
            },
            ..Default::default()
        },
    )
    .expect("policy should save");
    let _ = fs::remove_file(&log_path);

    let _ = list_server_tools_uncached("mock", &config, &workspace)
        .await
        .expect("tools/list should succeed while server is disabled for session");
    let disabled_log = fs::read_to_string(&log_path).expect("log should read");
    assert!(
        !disabled_log.contains("\"roots\""),
        "roots must not be advertised to servers disabled in the session policy"
    );

    save_session_policy(
        &workspace,
        &McpSessionPolicy {
            enabled_servers: HashSet::from(["mock".to_string()]),
            client_capabilities: McpClientCapabilityPolicy {
                roots: true,
                sampling: true,
                elicitation: true,
            },
            ..Default::default()
        },
    )
    .expect("policy should save");
    let _ = fs::remove_file(&log_path);

    let _ = list_server_tools_uncached("mock", &config, &workspace)
        .await
        .expect("tools/list should succeed after roots enabled");
    let second_log = fs::read_to_string(&log_path).expect("log should read");
    assert!(second_log.contains("\"roots\""));
    assert!(
        !second_log.contains("\"sampling\""),
        "sampling is not implemented and must not be advertised"
    );
    assert!(
        !second_log.contains("\"elicitation\""),
        "elicitation is not implemented and must not be advertised"
    );

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn stdio_server_request_id_collision_does_not_replace_client_response() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-roots-id-collision");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("roots-id-collision", &log_path);
    save_session_policy(
        &workspace,
        &McpSessionPolicy {
            enabled_servers: HashSet::from(["mock".to_string()]),
            client_capabilities: McpClientCapabilityPolicy {
                roots: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .expect("policy should save");

    let tools = list_server_tools_uncached("mock", &config, &workspace)
        .await
        .expect("tools/list should ignore same-id server requests and wait for response");

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].raw_name, "alpha");

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn server_timeout_defaults_to_tool_timeout_when_override_missing() {
    let mut config = test_config_with_mcp();
    config.exec_timeout = Duration::from_secs(7);
    config.tool_timeout = Duration::from_secs(45);
    config
        .mcp_servers
        .get_mut("github")
        .expect("github server should exist")
        .timeout_secs = None;

    let server = config
        .mcp_servers
        .get("github")
        .expect("github server should exist");

    assert_eq!(server_timeout_secs(server, &config), 45);
}

#[test]
fn should_reset_mcp_session_matches_transport_failures() {
    assert!(should_reset_mcp_session(
        "MCP initialize timed out after 5s"
    ));
    assert!(should_reset_mcp_session("MCP server closed stdout"));
    assert!(should_reset_mcp_session("failed to spawn 'npx': not found"));
    let localized_broken_pipe = std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "localized operating-system message",
    );
    let stable_error = format_mcp_stdio_transport_error("write", &localized_broken_pipe);
    assert!(stable_error.starts_with(MCP_STDIO_TRANSPORT_ERROR_PREFIX));
    assert!(stable_error.contains("BrokenPipe"));
    assert!(should_reset_mcp_session(&stable_error));
    assert!(!should_reset_mcp_session(
        "{\"code\":-32602,\"message\":\"invalid args\"}"
    ));
}

#[test]
fn resolve_server_cwd_rejects_workspace_escape() {
    let workspace = std::env::temp_dir().join("lingclaw-mcp-cwd-test");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let server = JsonMcpServerConfig {
        transport: None,
        command: "npx".to_string(),
        url: None,
        args: vec![],
        env: HashMap::new(),
        headers: HashMap::new(),
        cwd: Some("..".to_string()),
        enabled: true,
        auth: None,
        timeout_secs: None,
    };

    let err = resolve_server_cwd(&server, &workspace).expect_err("workspace escape must fail");
    assert!(err.contains("outside the session workspace"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn resolve_server_command_falls_back_to_home_local_bin() {
    let temp_home = std::env::temp_dir().join("lingclaw-mcp-command-home-test");
    let local_bin = temp_home.join(".local").join("bin");
    std::fs::create_dir_all(&local_bin).expect("local bin should be created");

    let command_name = if cfg!(windows) { "uvx.exe" } else { "uvx" };
    let command_path = local_bin.join(command_name);
    std::fs::write(&command_path, b"echo test").expect("command file should be written");

    let resolved = resolve_server_command_from_env(
        "uvx",
        Some(OsString::from("")),
        Some(temp_home.clone().into_os_string()),
        None,
    );

    if cfg!(windows) {
        assert_eq!(
            resolved.to_string_lossy().to_ascii_lowercase(),
            command_path.to_string_lossy().to_ascii_lowercase()
        );
    } else {
        assert_eq!(resolved, command_path);
    }

    let _ = std::fs::remove_dir_all(&temp_home);
}

#[test]
fn resolve_server_command_keeps_explicit_paths() {
    let explicit = if cfg!(windows) {
        r"C:\tools\uvx.exe"
    } else {
        "/usr/local/bin/uvx"
    };

    let resolved = resolve_server_command_from_env(explicit, Some(OsString::from("")), None, None);

    assert_eq!(resolved, PathBuf::from(explicit));
}

#[test]
fn format_mcp_timeout_error_includes_phase_and_diagnostics() {
    let error = format_mcp_timeout_error(
        "tools/list",
        120,
        &["Starting Minimax MCP server".to_string()],
        &["Traceback: missing key".to_string()],
    );

    assert!(error.contains("MCP tools/list timed out after 120s"));
    assert!(error.contains("stdout: Starting Minimax MCP server"));
    assert!(error.contains("stderr: Traceback: missing key"));
}

#[test]
fn push_diagnostic_line_trims_and_limits_buffer() {
    let mut lines = Vec::new();
    for index in 0..8 {
        push_diagnostic_line(&mut lines, &format!("line-{index}"));
    }

    assert_eq!(lines.len(), MCP_DIAGNOSTIC_LINE_LIMIT);
    assert_eq!(lines.first().map(String::as_str), Some("line-2"));
    assert_eq!(lines.last().map(String::as_str), Some("line-7"));
}

#[test]
fn write_message_uses_newline_delimited_jsonrpc() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let bytes = rt.block_on(async {
        write_message_for_test(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-11-25"}
        }))
        .await
        .expect("message should be written")
    });

    let output = String::from_utf8(bytes).expect("output should be utf-8");
    assert!(output.ends_with('\n'));
    assert!(!output.contains("Content-Length:"));
    assert!(output.trim_end().starts_with('{'));
}

#[test]
fn read_message_accepts_newline_delimited_jsonrpc_and_ignores_noise() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let message = rt.block_on(async {
        let (mut writer, reader) = tokio::io::duplex(512);
        let payload = json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}}).to_string();
        let frame = format!("Starting Minimax MCP server\n{}\n", payload);
        let writer_task = tokio::spawn(async move {
            writer
                .write_all(frame.as_bytes())
                .await
                .expect("frame should be written");
        });
        let stdout_lines = Arc::new(Mutex::new(Vec::new()));
        let mut reader = BufReader::new(reader);
        let message = read_message(&mut reader, &stdout_lines)
            .await
            .expect("message should parse");
        writer_task.await.expect("writer task should finish");
        let diagnostics = snapshot_diagnostic_lines(&stdout_lines);
        (message, diagnostics)
    });

    assert_eq!(message.0.get("id").and_then(Value::as_u64), Some(1));
    assert_eq!(message.1, vec!["Starting Minimax MCP server".to_string()]);
}

#[test]
fn read_message_keeps_legacy_content_length_compatibility() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let message = rt.block_on(async {
        let (mut writer, reader) = tokio::io::duplex(512);
        let payload = json!({"jsonrpc": "2.0", "id": 2, "result": {"ok": true}}).to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{}", payload.len(), payload);
        let writer_task = tokio::spawn(async move {
            writer
                .write_all(frame.as_bytes())
                .await
                .expect("frame should be written");
        });
        let stdout_lines = Arc::new(Mutex::new(Vec::new()));
        let mut reader = BufReader::new(reader);
        let message = read_message(&mut reader, &stdout_lines)
            .await
            .expect("message should parse");
        writer_task.await.expect("writer task should finish");
        message
    });

    assert_eq!(message.get("id").and_then(Value::as_u64), Some(2));
}

#[test]
fn read_response_handles_ping_requests_while_waiting_for_expected_id() {
    let workspace = unique_temp_workspace("lingclaw-mcp-ping-roots");
    fs::create_dir_all(&workspace).expect("create ping workspace");
    let workspace_root = resolve_path_checked(".", &workspace).expect("resolve ping workspace");
    let workspace_child_root = workspace_root
        .export_to_child()
        .expect("export ping workspace");
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let result = rt.block_on(async move {
        let (mut server_stdout, reader) = tokio::io::duplex(1024);
        let (mut client_stdin, server_stdin) = tokio::io::duplex(1024);
        let writer_task = tokio::spawn(async move {
            let ping = json!({"jsonrpc": "2.0", "id": "ping-1", "method": "ping"});
            let response = json!({"jsonrpc": "2.0", "id": 2, "result": {"tools": []}});
            server_stdout
                .write_all(format!("{}\n{}\n", ping, response).as_bytes())
                .await
                .expect("messages should be written");
        });

        let stdout_lines = Arc::new(Mutex::new(Vec::new()));
        let mut reader = BufReader::new(reader);
        let mut stdin_reader = BufReader::new(server_stdin);
        let response = read_response(
            &mut reader,
            &mut client_stdin,
            2,
            &stdout_lines,
            "github",
            &workspace_child_root,
            "cache-key",
            &McpClientCapabilityPolicy::default(),
        )
        .await
        .expect("expected response should be returned");

        let mut ping_reply = String::new();
        stdin_reader
            .read_line(&mut ping_reply)
            .await
            .expect("ping reply should be readable");
        writer_task.await.expect("writer task should finish");

        (
            response,
            ping_reply,
            snapshot_diagnostic_lines(&stdout_lines),
        )
    });
    drop(workspace_root);
    fs::remove_dir_all(&workspace).expect("clean ping workspace");

    assert_eq!(result.0.get("id").and_then(Value::as_u64), Some(2));
    assert!(result.1.contains("\"id\":\"ping-1\""));
    assert!(result.1.contains("\"result\":{}"));
    assert!(
        result
            .2
            .iter()
            .any(|line| line.contains("\"method\":\"ping\""))
    );
}

#[test]
fn read_response_handles_roots_list_requests_while_waiting_for_expected_id() {
    let workspace = unique_temp_workspace("lingclaw-mcp-roots-list");
    fs::create_dir_all(&workspace).expect("create roots workspace");
    let workspace_root = resolve_path_checked(".", &workspace).expect("resolve roots workspace");
    let workspace_child_root = workspace_root
        .export_to_child()
        .expect("export roots workspace");
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let result = rt.block_on(async move {
        let (mut server_stdout, reader) = tokio::io::duplex(1024);
        let (mut client_stdin, server_stdin) = tokio::io::duplex(1024);
        let writer_task = tokio::spawn(async move {
            let roots_list = json!({"jsonrpc": "2.0", "id": 7, "method": "roots/list"});
            let response = json!({"jsonrpc": "2.0", "id": 2, "result": {"tools": []}});
            server_stdout
                .write_all(format!("{}\n{}\n", roots_list, response).as_bytes())
                .await
                .expect("messages should be written");
        });

        let stdout_lines = Arc::new(Mutex::new(Vec::new()));
        let mut reader = BufReader::new(reader);
        let mut stdin_reader = BufReader::new(server_stdin);
        let response = read_response(
            &mut reader,
            &mut client_stdin,
            2,
            &stdout_lines,
            "github",
            &workspace_child_root,
            "cache-key",
            &McpClientCapabilityPolicy {
                roots: true,
                sampling: false,
                elicitation: false,
            },
        )
        .await
        .expect("expected response should be returned");

        let mut roots_reply = String::new();
        stdin_reader
            .read_line(&mut roots_reply)
            .await
            .expect("roots reply should be readable");
        writer_task.await.expect("writer task should finish");
        (response, roots_reply)
    });
    drop(workspace_root);
    fs::remove_dir_all(&workspace).expect("clean roots workspace");

    assert_eq!(result.0.get("id").and_then(Value::as_u64), Some(2));
    assert!(result.1.contains("\"id\":7"));
    assert!(result.1.contains("\"roots\""));
    assert!(result.1.contains("file://"));
}

#[tokio::test(flavor = "current_thread")]
async fn stdio_roots_response_stays_bound_after_validation_before_write() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let fixture = unique_temp_workspace("lingclaw-mcp-roots-write-barrier");
    cleanup.track_path(fixture.clone());
    let workspace = fixture.join("workspace");
    let moved = fixture.join("workspace-original");
    let outside = fixture.join("outside");
    fs::create_dir_all(&workspace).expect("create roots barrier workspace");
    fs::create_dir_all(&outside).expect("create roots barrier outside directory");
    fs::write(workspace.join("identity.txt"), "inside").expect("seed checked roots identity");
    fs::write(outside.join("identity.txt"), "outside").expect("seed outside roots identity");
    let checked = resolve_path_checked(".", &workspace).expect("resolve roots barrier workspace");
    let child_root = checked
        .export_to_child()
        .expect("export roots barrier capability");
    let (mut client_stdin, server_stdin) = tokio::io::duplex(4096);
    let mut server_reader = BufReader::new(server_stdin);
    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let mut replacement_installed = false;
    #[cfg(windows)]
    let mut move_was_blocked = false;

    handle_server_message_with_before_write_hook(
        &mut client_stdin,
        &json!({"jsonrpc": "2.0", "id": 71, "method": "roots/list"}),
        &diagnostics,
        "mock",
        &child_root,
        "roots-write-barrier",
        &McpClientCapabilityPolicy {
            roots: true,
            ..Default::default()
        },
        &mut || match fs::rename(&workspace, &moved) {
            Ok(()) => {
                #[cfg(unix)]
                std::os::unix::fs::symlink(&outside, &workspace)
                    .expect("install outside roots symlink at write barrier");
                #[cfg(windows)]
                {
                    let output = StdCommand::new("cmd.exe")
                        .arg("/c")
                        .arg("mklink")
                        .arg("/J")
                        .arg(&workspace)
                        .arg(&outside)
                        .output()
                        .expect("run roots barrier junction command");
                    assert!(
                        output.status.success(),
                        "roots barrier junction should install: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                replacement_installed = true;
            }
            Err(error) => {
                #[cfg(windows)]
                {
                    assert!(
                        error.kind() == std::io::ErrorKind::PermissionDenied
                            || error.raw_os_error() == Some(32),
                        "unexpected Windows roots lock error: {error}"
                    );
                    move_was_blocked = true;
                }
                #[cfg(not(windows))]
                panic!("move roots workspace at response barrier: {error}");
            }
        },
    )
    .await
    .expect("roots response should remain capability-bound");

    let mut reply = String::new();
    server_reader
        .read_line(&mut reply)
        .await
        .expect("read capability-bound roots response");
    let reply: Value = serde_json::from_str(&reply).expect("parse roots response");
    let uri = reply["result"]["roots"][0]["uri"]
        .as_str()
        .expect("roots response URI");
    assert_eq!(uri, path_to_file_uri(child_root.path()));
    assert_eq!(
        fs::read_to_string(child_root.path().join("identity.txt"))
            .expect("read identity through exported root capability"),
        "inside",
        "the emitted root must still resolve only to the original capability"
    );
    assert!(
        !uri.contains(&outside.to_string_lossy().replace('\\', "/")),
        "roots response must never reveal the replacement path: {uri}"
    );
    #[cfg(unix)]
    assert!(
        replacement_installed,
        "Unix barrier must replace the root path"
    );
    #[cfg(windows)]
    assert!(
        replacement_installed || move_was_blocked,
        "Windows barrier must either retain the original capability or block replacement"
    );

    if replacement_installed {
        #[cfg(unix)]
        fs::remove_file(&workspace).expect("remove roots barrier symlink");
        #[cfg(windows)]
        fs::remove_dir(&workspace).expect("remove roots barrier junction");
    }
    drop(child_root);
    drop(checked);
    if moved.exists() {
        fs::rename(&moved, &workspace).expect("restore roots barrier workspace");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_child_reads_only_the_exported_root_after_the_send_barrier() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let fixture = unique_temp_workspace("lingclaw-mcp-child-roots-barrier");
    cleanup.track_path(fixture.clone());
    let workspace = fixture.join("workspace");
    let moved = fixture.join("workspace-original");
    let outside = fixture.join("outside");
    let log_path = fixture.join("mock.log");
    fs::create_dir_all(&workspace).expect("create child roots workspace");
    fs::create_dir_all(&outside).expect("create child roots outside directory");
    fs::write(workspace.join("identity.txt"), "inside").expect("seed child roots inside identity");
    fs::write(outside.join("identity.txt"), "outside").expect("seed child roots outside identity");
    let config = test_config_with_mock_server("verify-roots-capability", &log_path);
    let (_, session) = get_or_create_server_session("mock", &config, &workspace)
        .await
        .expect("spawn roots-verifying stdio child");
    let roots_capabilities = McpClientCapabilityPolicy {
        roots: true,
        ..Default::default()
    };
    let mut replacement_installed = false;
    #[cfg(windows)]
    let mut move_was_blocked = false;

    {
        let mut session = session.lock().await;
        let McpServerSession {
            stdin,
            stdout_lines,
            server_name,
            workspace_child_root,
            tool_cache_key,
            ..
        } = &mut *session;
        handle_server_message_with_before_write_hook(
            stdin,
            &json!({"jsonrpc": "2.0", "id": 9100, "method": "roots/list"}),
            stdout_lines,
            server_name,
            workspace_child_root,
            tool_cache_key,
            &roots_capabilities,
            &mut || match fs::rename(&workspace, &moved) {
                Ok(()) => {
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&outside, &workspace)
                        .expect("install child roots outside symlink");
                    #[cfg(windows)]
                    {
                        let output = StdCommand::new("cmd.exe")
                            .arg("/c")
                            .arg("mklink")
                            .arg("/J")
                            .arg(&workspace)
                            .arg(&outside)
                            .output()
                            .expect("run child roots junction command");
                        assert!(output.status.success());
                    }
                    replacement_installed = true;
                }
                Err(error) => {
                    #[cfg(windows)]
                    {
                        assert!(
                            error.kind() == std::io::ErrorKind::PermissionDenied
                                || error.raw_os_error() == Some(32),
                            "unexpected Windows child-root lock error: {error}"
                        );
                        move_was_blocked = true;
                    }
                    #[cfg(not(windows))]
                    panic!("move child roots workspace at send barrier: {error}");
                }
            },
        )
        .await
        .expect("write roots response to the stdio child");
        wait_for_file_log_line(&log_path, "roots-identity:").await;
    }

    let log = fs::read_to_string(&log_path).expect("read child roots log");
    assert!(log.contains("roots-identity:inside"), "{log}");
    assert!(!log.contains("roots-identity:outside"), "{log}");
    assert_eq!(
        fs::read_to_string(outside.join("identity.txt")).expect("read outside identity"),
        "outside"
    );
    #[cfg(unix)]
    assert!(
        replacement_installed,
        "Unix child-root barrier must replace the path"
    );
    #[cfg(windows)]
    assert!(
        replacement_installed || move_was_blocked,
        "Windows child-root barrier must preserve the root lock"
    );

    if replacement_installed {
        #[cfg(unix)]
        fs::remove_file(&workspace).expect("remove child roots symlink");
        #[cfg(windows)]
        fs::remove_dir(&workspace).expect("remove child roots junction");
    }
    drop(session);
    clear_mcp_caches_for_test().await;
    if moved.exists() {
        fs::rename(&moved, &workspace).expect("restore child roots workspace");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn tools_list_changed_notification_invalidates_cached_descriptors() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-tool-change");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("tool-change", &log_path);

    let first = list_server_tools("mock", &config, &workspace)
        .await
        .expect("first tools/list should succeed");
    assert_eq!(first[0].raw_name, "alpha");

    call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {}}),
    )
    .await
    .expect("tools/call should consume invalidation notification");

    let second = list_server_tools("mock", &config, &workspace)
        .await
        .expect("second tools/list should refetch after invalidation");
    assert_eq!(second[0].raw_name, "beta");

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn call_server_restarts_cached_session_after_server_exit() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-restart");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("restart-once", &log_path);

    let first = call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {"value": "one"}}),
    )
    .await
    .expect("first tools/call should succeed");
    assert_eq!(first["content"][0]["text"], "ok");

    let second = call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {"value": "two"}}),
    )
    .await
    .expect("second tools/call should respawn session and succeed");
    assert_eq!(second["content"][0]["text"], "ok");
    assert_eq!(log_line_count(&log_path, "start"), 2);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn refresh_servers_clears_cached_tools_and_sessions() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-refresh");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("default", &log_path);

    let _ = list_server_tools("mock", &config, &workspace)
        .await
        .expect("tools should load");
    let _ = call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {}}),
    )
    .await
    .expect("session should be created");

    assert_eq!(tool_cache().lock().expect("tool cache lock").len(), 1);
    assert_eq!(session_cache().lock().expect("session cache lock").len(), 1);

    let reports = refresh_servers(&config, &workspace)
        .await
        .expect("refresh should succeed");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].server_name, "mock");
    assert_eq!(tool_cache().lock().expect("tool cache lock").len(), 1);
    assert_eq!(session_cache().lock().expect("session cache lock").len(), 0);
    assert_eq!(log_line_count(&log_path, "start"), 4);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn invalidate_runtime_state_without_remote_shutdown_does_not_delete_http_session() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let (url, log, handle) = spawn_auth_recording_streamable_http_test_server().await;
    let workspace = unique_temp_workspace("lingclaw-mcp-local-invalidate-no-delete");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let config = test_config_with_streamable_http_server(url);

    let tools = list_server_tools("http", &config, &workspace)
        .await
        .expect("tools should load");
    assert_eq!(tools.len(), 1);
    assert_eq!(tool_cache().lock().expect("tool cache lock").len(), 1);
    assert_eq!(
        http_session_cache()
            .lock()
            .expect("HTTP session cache lock")
            .len(),
        1
    );
    let calls_before = log.lock().await.len();

    invalidate_runtime_state_without_remote_shutdown().await;

    let calls_after = log.lock().await.clone();
    assert_eq!(
        calls_after.len(),
        calls_before,
        "local invalidation should not contact the remote MCP server"
    );
    assert!(
        calls_after.iter().all(|call| call["method"] != "DELETE"),
        "local invalidation should not terminate remote HTTP sessions: {calls_after:?}"
    );
    assert_eq!(tool_cache().lock().expect("tool cache lock").len(), 0);
    assert_eq!(
        http_session_cache()
            .lock()
            .expect("HTTP session cache lock")
            .len(),
        0
    );

    let _ = list_server_tools("http", &config, &workspace)
        .await
        .expect("tools should reload after local invalidation");
    let calls_after_reload = log.lock().await.clone();
    let initialize_count = calls_after_reload
        .iter()
        .filter(|call| call["method"] == "initialize")
        .count();
    assert_eq!(
        initialize_count, 2,
        "next MCP use should build a fresh HTTP session"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn reap_idle_server_sessions_removes_stale_entries() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-idle");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("default", &log_path);

    let (cache_key, _) = get_or_create_server_session("mock", &config, &workspace)
        .await
        .expect("session should be created");
    {
        let mut cache = session_cache().lock().expect("session cache lock");
        let entry = cache
            .get_mut(&cache_key)
            .expect("cached session should exist");
        entry.last_used_at = Instant::now() - session_idle_ttl() - Duration::from_secs(1);
    }

    reap_idle_server_sessions(Instant::now())
        .await
        .expect("idle reap should succeed");
    assert_eq!(session_cache().lock().expect("session cache lock").len(), 0);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_calls_share_cached_session() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-concurrent");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("concurrent", &log_path);

    call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {"value": "warmup"}}),
    )
    .await
    .expect("warmup call should succeed");
    assert_eq!(log_line_count(&log_path, "start"), 1);

    let left = call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {"value": "left"}}),
    );
    let right = call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {"value": "right"}}),
    );

    let (left, right) = tokio::join!(left, right);
    assert_eq!(
        left.expect("left call should succeed")["content"][0]["text"],
        "left"
    );
    assert_eq!(
        right.expect("right call should succeed")["content"][0]["text"],
        "right"
    );
    assert_eq!(log_line_count(&log_path, "start"), 1);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn cached_stdio_session_rejects_a_moved_workspace_identity() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let workspace = unique_temp_workspace("lingclaw-mcp-cached-root");
    let moved = workspace.with_extension("moved");
    cleanup.track_path(moved.clone());
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create cached stdio workspace");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("default", &log_path);
    let (cache_key, session) = get_or_create_server_session("mock", &config, &workspace)
        .await
        .expect("create cached stdio session");

    match fs::rename(&workspace, &moved) {
        Ok(()) => {
            fs::create_dir(&workspace).expect("install replacement workspace");
            let result = get_or_create_server_session("mock", &config, &workspace).await;
            assert!(
                result
                    .as_ref()
                    .is_err_and(|error| error.contains("MCP workspace capability invalidated")),
                "moved cached workspace must fail closed"
            );
            assert!(
                !session_cache()
                    .lock()
                    .expect("stdio session cache lock")
                    .contains_key(&cache_key),
                "invalid stdio session must be evicted"
            );
            let mut session = session.lock().await;
            assert!(
                session
                    .child
                    .try_wait()
                    .expect("inspect mock child")
                    .is_some(),
                "invalid stdio session child must be closed"
            );
            assert!(
                workspace_roots_result("mock", &session.workspace_child_root).is_err(),
                "roots/list must not expose a replacement workspace"
            );
        }
        Err(error) => {
            #[cfg(windows)]
            {
                assert!(
                    error.kind() == std::io::ErrorKind::PermissionDenied
                        || error.raw_os_error() == Some(32),
                    "unexpected Windows rename error: {error}"
                );
                let (_, reused) = get_or_create_server_session("mock", &config, &workspace)
                    .await
                    .expect("locked Windows workspace should reuse its session");
                assert!(Arc::ptr_eq(&session, &reused));
            }
            #[cfg(not(windows))]
            panic!("rename cached stdio workspace: {error}");
        }
    }

    drop(session);
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
    let _ = fs::remove_dir_all(&moved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_roots_request_during_a_workspace_move_evicts_the_cached_session() {
    run_panic_safe_mcp_test!(cleanup, {
        let fixture = unique_temp_workspace("lingclaw-mcp-delayed-roots");
        cleanup.track_path(fixture.clone());
        let workspace = fixture.join("workspace");
        let moved = fixture.join("workspace-moved");
        fs::create_dir_all(&workspace).expect("create delayed roots workspace");
        let log_path = fixture.join("mock.log");
        let config = test_config_with_mock_server("delayed-roots", &log_path);
        let policy = McpSessionPolicy {
            enabled_servers: HashSet::from(["mock".to_string()]),
            client_capabilities: McpClientCapabilityPolicy {
                roots: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let (cache_key, session) =
            get_or_create_server_session_for_policy("mock", &config, &workspace, &policy)
                .await
                .expect("create roots-enabled cached stdio session");

        let request = {
            let config = config.clone();
            let workspace = workspace.clone();
            let policy = policy.clone();
            cleanup.spawn_worker(async move {
                call_server_for_policy(
                    "mock",
                    &config,
                    &workspace,
                    &policy,
                    "tools/call",
                    json!({"name": "alpha", "arguments": {}}),
                )
                .await
            })
        };
        wait_for_file_log_line(&log_path, "\"method\":\"tools/call\"").await;

        match fs::rename(&workspace, &moved) {
            Ok(()) => {
                fs::create_dir(&workspace).expect("install replacement stdio workspace");
                let result = request.await.expect("join delayed stdio request");
                assert!(
                    result
                        .as_ref()
                        .is_err_and(|error| error.contains("MCP workspace capability invalidated")),
                    "roots/list during a moved workspace must fail closed: {result:?}"
                );
                let log = fs::read_to_string(&log_path).expect("read delayed roots log");
                assert!(log.contains("send:roots/list"));
                assert!(
                    !log.contains("\"result\":{\"roots\""),
                    "the replacement root must not be returned to the stdio server: {log}"
                );
                assert!(
                    !session_cache()
                        .lock()
                        .expect("stdio session cache lock")
                        .contains_key(&cache_key),
                    "invalidated stdio session must be evicted"
                );
                let mut session = session.lock().await;
                assert!(
                    session
                        .child
                        .try_wait()
                        .expect("inspect delayed roots child")
                        .is_some(),
                    "invalidated stdio child must be closed"
                );
            }
            Err(error) => {
                #[cfg(windows)]
                {
                    assert!(
                        error.kind() == std::io::ErrorKind::PermissionDenied
                            || error.raw_os_error() == Some(32),
                        "unexpected Windows rename error: {error}"
                    );
                    request
                        .await
                        .expect("join locked Windows stdio request")
                        .expect("locked Windows workspace should answer roots/list safely");
                }
                #[cfg(not(windows))]
                panic!("rename delayed roots workspace: {error}");
            }
        }

        drop(session);
    });
}

#[tokio::test(flavor = "current_thread")]
async fn cached_http_session_rejects_a_moved_workspace_and_closes_remote_session() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let mut cleanup = PanicSafeMcpFixture::default();
    let (url, log, handle) = spawn_auth_recording_streamable_http_test_server().await;
    cleanup.track_task(handle);
    let workspace = unique_temp_workspace("lingclaw-http-cached-root");
    let moved = workspace.with_extension("moved");
    cleanup.track_path(moved.clone());
    cleanup.track_path(workspace.clone());
    fs::create_dir_all(&workspace).expect("create cached HTTP workspace");
    let config = test_config_with_streamable_http_server(url);
    call_http_server("http", &config, &workspace, "tools/list", json!({}))
        .await
        .expect("warm cached HTTP session");
    assert_eq!(
        http_session_cache().lock().expect("HTTP cache lock").len(),
        1
    );

    match fs::rename(&workspace, &moved) {
        Ok(()) => {
            fs::create_dir(&workspace).expect("install replacement HTTP workspace");
            let result =
                call_http_server("http", &config, &workspace, "tools/list", json!({})).await;
            assert!(
                result
                    .as_ref()
                    .is_err_and(|error| error.contains("MCP workspace capability invalidated")),
                "moved cached HTTP workspace must fail closed: {result:?}"
            );
            assert!(
                http_session_cache()
                    .lock()
                    .expect("HTTP cache lock")
                    .is_empty(),
                "invalid HTTP session must be evicted"
            );
            assert!(
                http_stream_tasks()
                    .lock()
                    .expect("HTTP stream task lock")
                    .is_empty(),
                "invalid HTTP stream must be aborted"
            );
            let calls = log.lock().await.clone();
            assert!(
                calls.iter().any(|call| call["method"] == "DELETE"),
                "invalid HTTP session must be closed remotely: {calls:?}"
            );
        }
        Err(error) => {
            #[cfg(windows)]
            {
                assert!(
                    error.kind() == std::io::ErrorKind::PermissionDenied
                        || error.raw_os_error() == Some(32)
                );
                call_http_server("http", &config, &workspace, "tools/list", json!({}))
                    .await
                    .expect("locked Windows HTTP workspace should reuse its session");
            }
            #[cfg(not(windows))]
            panic!("rename cached HTTP workspace: {error}");
        }
    }

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
    let _ = fs::remove_dir_all(&moved);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_roots_request_during_a_workspace_move_closes_the_cached_session() {
    run_panic_safe_mcp_test!(cleanup, {
        let (url, log, handle) = spawn_delayed_roots_streamable_http_test_server().await;
        cleanup.track_task(handle);
        let fixture = unique_temp_workspace("lingclaw-http-delayed-roots");
        cleanup.track_path(fixture.clone());
        let workspace = fixture.join("workspace");
        let moved = fixture.join("workspace-moved");
        fs::create_dir_all(&workspace).expect("create delayed HTTP roots workspace");
        let config = test_config_with_streamable_http_server(url);
        let policy = McpSessionPolicy {
            enabled_servers: HashSet::from(["http".to_string()]),
            client_capabilities: McpClientCapabilityPolicy {
                roots: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let request = {
            let config = config.clone();
            let workspace = workspace.clone();
            let policy = policy.clone();
            cleanup.spawn_worker(async move {
                call_http_server_for_policy(
                    "http",
                    &config,
                    &workspace,
                    &policy,
                    "tools/list",
                    json!({}),
                )
                .await
            })
        };
        wait_for_http_method(&log, "tools/list").await;

        match fs::rename(&workspace, &moved) {
            Ok(()) => {
                fs::create_dir(&workspace).expect("install replacement HTTP workspace");
                let result = request.await.expect("join delayed HTTP request");
                assert!(
                    result
                        .as_ref()
                        .is_err_and(|error| error.contains("MCP workspace capability invalidated")),
                    "HTTP roots/list during a moved workspace must fail closed: {result:?}"
                );
                assert!(
                    http_session_cache()
                        .lock()
                        .expect("HTTP cache lock")
                        .is_empty(),
                    "invalidated HTTP session must be evicted"
                );
                let calls = log.lock().await.clone();
                assert!(
                    calls.iter().any(|call| call["method"] == "DELETE"),
                    "invalidated HTTP session must be closed remotely: {calls:?}"
                );
                assert!(
                    !calls.iter().any(|call| {
                        call["payload"]["result"]["roots"]
                            .as_array()
                            .is_some_and(|roots| !roots.is_empty())
                    }),
                    "the replacement root must not be returned to the HTTP server: {calls:?}"
                );
            }
            Err(error) => {
                #[cfg(windows)]
                {
                    assert!(
                        error.kind() == std::io::ErrorKind::PermissionDenied
                            || error.raw_os_error() == Some(32),
                        "unexpected Windows rename error: {error}"
                    );
                    request
                        .await
                        .expect("join locked Windows HTTP request")
                        .expect("locked Windows workspace should answer roots/list safely");
                }
                #[cfg(not(windows))]
                panic!("rename delayed HTTP roots workspace: {error}");
            }
        }
    });
}

#[tokio::test(flavor = "current_thread")]
async fn isolated_mcp_calls_use_separate_sessions() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-isolated");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("concurrent", &log_path);

    let reports = refresh_servers(&config, &workspace)
        .await
        .expect("mock MCP server should refresh");
    let tool_name = reports[0]
        .tool_names
        .first()
        .cloned()
        .expect("mock MCP server should expose a tool");

    let _ = fs::remove_file(&log_path);

    let left = execute_tool_isolated(&tool_name, r#"{"value":"left"}"#, &config, &workspace);
    let right = execute_tool_isolated(&tool_name, r#"{"value":"right"}"#, &config, &workspace);

    let (left, right) = tokio::join!(left, right);
    assert_eq!(left.expect("left call should succeed").output, "left");
    assert_eq!(right.expect("right call should succeed").output, "right");
    assert_eq!(log_line_count(&log_path, "start"), 2);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn mutating_mcp_tools_are_not_parallelizable() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-mutating");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("mutating", &log_path);

    let reports = refresh_servers(&config, &workspace)
        .await
        .expect("mock MCP server should refresh");
    let tool_name = reports[0]
        .tool_names
        .first()
        .cloned()
        .expect("mock MCP server should expose a tool");

    assert!(!crate::tools::is_parallelizable_tool_call(
        &tool_name, &config, &workspace
    ));

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn confirmation_policy_blocks_mutating_mcp_tools_before_execution() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-mutating-confirmation");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("mutating", &log_path);

    let reports = refresh_servers(&config, &workspace)
        .await
        .expect("mock MCP server should refresh");
    let tool_name = reports[0]
        .tool_names
        .first()
        .cloned()
        .expect("mock MCP server should expose a tool");
    let policy = McpSessionPolicy {
        enabled_servers: HashSet::from(["mock".to_string()]),
        enabled_tools: HashSet::from([tool_name.clone()]),
        confirm_mutating_tools: true,
        client_capabilities: Default::default(),
        cache_namespace: None,
    };

    let outcome = execute_tool_for_policy(&tool_name, "{}", &config, &workspace, false, &policy)
        .await
        .expect("MCP tool should return an outcome");

    assert!(outcome.is_error);
    assert!(outcome.output.contains("requires confirmation"));
    assert_eq!(log_line_count(&log_path, "tools/call"), 0);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn inspect_servers_returns_reports_in_sorted_order() {
    let mut config = test_config_with_mcp();
    config.mcp_servers.insert(
        "alpha".to_string(),
        JsonMcpServerConfig {
            transport: None,
            command: "definitely-not-a-real-command".to_string(),
            url: None,
            args: vec![],
            env: HashMap::new(),
            headers: HashMap::new(),
            cwd: None,
            enabled: true,
            auth: None,
            timeout_secs: Some(1),
        },
    );
    config
        .mcp_servers
        .get_mut("github")
        .expect("github server should exist")
        .command = "definitely-not-a-real-command".to_string();

    let workspace = std::env::temp_dir().join("lingclaw-mcp-inspect-order-test");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let reports = rt.block_on(async { inspect_servers(&config, &workspace).await });

    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].server_name, "alpha");
    assert_eq!(reports[1].server_name, "github");

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn path_to_file_uri_encodes_spaces_and_non_ascii() {
    let uri = path_to_file_uri(Path::new("/tmp/my workspace"));
    assert_eq!(uri, "file:///tmp/my%20workspace");

    let uri_cn = path_to_file_uri(Path::new("/home/鐢ㄦ埛/workspace"));
    assert!(uri_cn.starts_with("file:///home/"));
    assert!(
        !uri_cn.contains("鐢ㄦ埛"),
        "non-ASCII chars must be percent-encoded"
    );
    assert!(
        uri_cn.contains('%'),
        "non-ASCII bytes must be percent-encoded"
    );
}

#[test]
fn spawn_cooldown_blocks_rapid_retry() {
    let server = "test_cooldown_server";
    // Clear any existing state.
    clear_spawn_failure(server);
    assert!(check_spawn_cooldown(server).is_none());

    // Record failure and verify cooldown is active.
    record_spawn_failure(server);
    let remaining = check_spawn_cooldown(server);
    assert!(
        remaining.is_some(),
        "cooldown should be active after failure"
    );
    assert!(remaining.unwrap() > 0);

    // Clear and verify cooldown is gone.
    clear_spawn_failure(server);
    assert!(check_spawn_cooldown(server).is_none());
}
