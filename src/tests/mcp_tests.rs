use super::*;
use crate::{DEFAULT_PORT, Provider, config::JsonMcpAuthConfig};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{Form, State},
    http::{HeaderMap, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures::stream;
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
        "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\",\"params\":{}}\n\n",
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
    reset_auth_file_path_for_test();
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
    if let Ok(mut ids) = http_last_event_ids().lock() {
        ids.clear();
    }
    if let Ok(mut locks) = http_initialization_locks().lock() {
        locks.clear();
    }
    let http_tasks = match http_stream_tasks().lock() {
        Ok(mut tasks) => tasks
            .drain()
            .map(|(_, entry)| entry.handle)
            .collect::<Vec<_>>(),
        Err(_) => Vec::new(),
    };
    for task in http_tasks {
        task.abort();
    }

    let sessions = {
        let Ok(mut cache) = session_cache().lock() else {
            return;
        };
        cache
            .drain()
            .map(|(_, entry)| entry.session)
            .collect::<Vec<_>>()
    };

    for session in sessions {
        let mut guard = session.lock().await;
        guard.shutdown().await;
    }
}

fn log_line_count(log_path: &Path, needle: &str) -> usize {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(needle))
        .count()
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

#[test]
fn sse_parser_tracks_last_event_id_and_invalidates_caches_for_notifications() {
    let cache_key = "sse-cache-key";
    {
        let mut cache = tool_cache().lock().expect("tool cache lock");
        cache.insert(
            cache_key.to_string(),
            CachedToolDescriptors {
                descriptors: Vec::new(),
                loaded_at: Instant::now(),
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
    assert_eq!(http_last_event_id(cache_key).as_deref(), Some("42"));
    assert!(
        !tool_cache()
            .lock()
            .expect("tool cache lock")
            .contains_key(cache_key)
    );

    remove_http_session(cache_key);
}

#[tokio::test]
async fn expired_http_session_removes_stream_task() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let cache_key = "expired-http-session";
    if let Ok(mut cache) = http_session_cache().lock() {
        cache.insert(
            cache_key.to_string(),
            CachedHttpMcpSession {
                session_id: Some("old-session".to_string()),
                last_used_at: Instant::now() - session_idle_ttl() - Duration::from_secs(1),
            },
        );
    }
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    if let Ok(mut tasks) = http_stream_tasks().lock() {
        tasks.insert(
            cache_key.to_string(),
            HttpStreamTaskEntry {
                task_id: next_http_stream_task_id(),
                handle: tokio::spawn(async move {
                    let _ = rx.await;
                }),
            },
        );
    }

    assert_eq!(http_session_id(cache_key), None);
    assert!(
        !http_stream_tasks()
            .lock()
            .expect("HTTP stream tasks lock")
            .contains_key(cache_key),
        "expired HTTP sessions should remove their stale stream task"
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
        },
    );
    http_session_cache()
        .lock()
        .expect("HTTP session cache lock")
        .insert(
            cache_key.clone(),
            CachedHttpMcpSession {
                session_id: Some("old-session".to_string()),
                last_used_at: Instant::now(),
            },
        );
    http_initialization_locks()
        .lock()
        .expect("HTTP init lock map")
        .insert(cache_key.clone(), Arc::new(AsyncMutex::new(())));

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
        !http_initialization_locks()
            .lock()
            .expect("HTTP init lock map")
            .contains_key(&cache_key),
        "OAuth completion should clear stale initialization locks for the server"
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
            call["payload"]["result"]["roots"]
                .as_array()
                .is_some_and(|roots| !roots.is_empty())
        }),
        "client should answer roots/list requests received on the HTTP SSE stream"
    );

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
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
                handle: replacement,
            },
        );
    }

    drop(HttpStreamTaskCleanup {
        cache_key: cache_key.clone(),
        task_id: replacement_task_id.saturating_sub(1),
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

    handle.abort();
    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
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
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let result = rt.block_on(async {
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
            Path::new("/tmp/workspace"),
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
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let result = rt.block_on(async {
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
        let workspace = if cfg!(windows) {
            PathBuf::from(r"C:\tmp\workspace root")
        } else {
            PathBuf::from("/tmp/workspace root")
        };
        let mut reader = BufReader::new(reader);
        let mut stdin_reader = BufReader::new(server_stdin);
        let response = read_response(
            &mut reader,
            &mut client_stdin,
            2,
            &stdout_lines,
            "github",
            &workspace,
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

    assert_eq!(result.0.get("id").and_then(Value::as_u64), Some(2));
    assert!(result.1.contains("\"id\":7"));
    assert!(result.1.contains("\"roots\""));
    assert!(result.1.contains("file://"));
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
