use super::*;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::atomic::AtomicU64,
};

use crate::config::{JsonMcpServerConfig, JsonModelEntry, JsonProviderConfig, S3Config};

fn test_config() -> Config {
    Config {
        explicit_primary_model_configured: true,
        provider_catalog_declared: false,
        api_key: "env-key".to_string(),
        api_base: "https://fallback.example/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::OpenAI,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: DEFAULT_PORT,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        tool_timeout: Duration::from_secs(30),
        sub_agent_timeout: Duration::from_secs(300),
        max_llm_retries: 2,
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
        openai_stream_include_usage: false,
        structured_memory: false,

        daily_reflection: false,
        s3: None,
        enable_state_digest: true,
        enable_task_plan: true,
        enable_groups: true,
    }
}

fn test_reasoning_config() -> Config {
    let mut config = test_config();
    config.model = "openai/reasoning-model".to_string();
    config.providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "reasoning-model".to_string(),
                reasoning: Some(true),
                ..Default::default()
            }],
        },
    );
    config
}

#[test]
fn delegated_config_uses_the_validated_session_model_as_primary_fallback() {
    let config = test_config();
    let delegated = delegated_config_for_run(&config, "openai/session-model");

    assert_eq!(delegated.model, "openai/session-model");
    assert_eq!(
        crate::subagents::executor::resolve_subagent_model(&delegated, "reviewer"),
        "openai/session-model"
    );

    let mut configured = config;
    configured.sub_agent_model = Some("openai/sub-agent-model".to_string());
    let delegated = delegated_config_for_run(&configured, "openai/session-model");
    assert_eq!(
        crate::subagents::executor::resolve_subagent_model(&delegated, "reviewer"),
        "openai/sub-agent-model"
    );
}

fn test_app_state() -> AppState {
    AppState {
        config: std::sync::Mutex::new(Arc::new(test_config())),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        group_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        session_control_locks: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: HookRegistry::new(),
        auxiliary_tasks: crate::auxiliary_tasks::AuxiliaryTaskRegistry::new(true, true),
        memory_queue: std::sync::Mutex::new(None),
    }
}

fn test_app_state_with_hooks(hooks: HookRegistry) -> AppState {
    AppState {
        config: std::sync::Mutex::new(Arc::new(test_config())),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        group_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        session_control_locks: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks,
        auxiliary_tasks: crate::auxiliary_tasks::AuxiliaryTaskRegistry::new(true, true),
        memory_queue: std::sync::Mutex::new(None),
    }
}

struct ThinkOverrideHook {
    new_level: String,
}

struct ForceCompressionHook;

impl crate::hooks::AgentHook for ForceCompressionHook {
    fn name(&self) -> &'static str {
        "force_compression"
    }

    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeAnalyze
    }

    fn should_run(&self, _: &[ChatMessage], _: Provider, _: usize, _: usize) -> bool {
        true
    }

    fn run<'a>(
        &'a self,
        input: crate::hooks::HookInput,
        _: &'a Config,
        _: &'a reqwest::Client,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::hooks::HookOutput> + Send + 'a>>
    {
        Box::pin(async move {
            let messages = crate::build_compressed_messages(&input.messages, 3, "forced summary");
            crate::hooks::HookOutput::ReplaceMessages {
                messages,
                events: vec![crate::hooks::build_context_compressed_event(
                    2, 10_000, 4_000, 320, false,
                )],
                usage: None,
            }
        })
    }
}

struct ForceCompressionSkippedHook;

impl crate::hooks::AgentHook for ForceCompressionSkippedHook {
    fn name(&self) -> &'static str {
        "force_compression_skipped"
    }

    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeAnalyze
    }

    fn should_run(&self, _: &[ChatMessage], _: Provider, _: usize, _: usize) -> bool {
        true
    }

    fn run<'a>(
        &'a self,
        _: crate::hooks::HookInput,
        _: &'a Config,
        _: &'a reqwest::Client,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::hooks::HookOutput> + Send + 'a>>
    {
        Box::pin(async move {
            crate::hooks::HookOutput::EmitEvents {
                events: vec![crate::hooks::build_context_compress_skipped_event(
                    "insufficient_savings",
                )],
                usage: Some(crate::context::UsageUpdate {
                    input_tokens: 123,
                    output_tokens: 45,
                    input_source: "provider".to_string(),
                    output_source: "provider".to_string(),
                    labels: crate::context::build_usage_labels(
                        123,
                        45,
                        Some("openai"),
                        Some(crate::context::USAGE_ROLE_CONTEXT),
                    ),
                }),
            }
        })
    }
}

impl crate::hooks::AgentHook for ThinkOverrideHook {
    fn name(&self) -> &'static str {
        "test_think_override"
    }

    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeLlmCall
    }

    fn should_run(&self, _: &[ChatMessage], _: Provider, _: usize, _: usize) -> bool {
        false
    }

    fn run<'a>(
        &'a self,
        _: crate::hooks::HookInput,
        _: &'a Config,
        _: &'a reqwest::Client,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::hooks::HookOutput> + Send + 'a>>
    {
        Box::pin(async { crate::hooks::HookOutput::NoOp })
    }

    fn should_run_llm(&self, _cycle: usize) -> bool {
        true
    }

    fn run_llm<'a>(
        &'a self,
        _: LlmHookInput,
        _: &'a Config,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = crate::hooks::HookOutput> + Send + 'a>>
    {
        let new_level = self.new_level.clone();
        Box::pin(async move {
            crate::hooks::HookOutput::ModifyLlmParams {
                extra_system: None,
                think_override: Some(new_level),
            }
        })
    }
}

#[test]
fn auto_think_support_treats_gemini3_as_reasoning_capable() {
    let resolved = providers::ResolvedModel {
        provider: Provider::Gemini,
        api_base: Provider::Gemini.default_api_base().into(),
        api_key: "gemini-key".into(),
        model_id: "gemini-3-flash-preview".into(),
        reasoning: false,
        thinking_format: None,
        openai_responses_reasoning_summary: None,
        max_tokens: Some(512),
        context_window: 1_000_000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };

    assert!(providers::auto_think_supported(&resolved));
}

async fn recv_json_with_timeout(rx: &mut tokio::sync::mpsc::Receiver<String>) -> serde_json::Value {
    let payload = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await
        .expect("timed out waiting for message")
        .expect("channel closed before message arrived");
    serde_json::from_str(&payload).expect("payload json")
}

type QueuedSseState = (
    Arc<Mutex<VecDeque<String>>>,
    Arc<Mutex<Vec<serde_json::Value>>>,
);

struct QueuedSseServerGuard {
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for QueuedSseServerGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

struct BlockingAuxiliaryProviderState {
    arrived: tokio::sync::Notify,
    release: tokio::sync::Notify,
    requests: Mutex<Vec<serde_json::Value>>,
    content: String,
}

async fn blocking_auxiliary_provider_handler(
    State(state): State<Arc<BlockingAuxiliaryProviderState>>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    state.requests.lock().await.push(request);
    state.arrived.notify_one();
    state.release.notified().await;
    Json(json!({
        "choices": [{"message": {"content": state.content.clone()}}],
        "usage": {"prompt_tokens": 17, "completion_tokens": 9}
    }))
    .into_response()
}

async fn spawn_blocking_auxiliary_provider(
    content: &str,
) -> (
    String,
    Arc<BlockingAuxiliaryProviderState>,
    QueuedSseServerGuard,
) {
    let state = Arc::new(BlockingAuxiliaryProviderState {
        arrived: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        requests: Mutex::new(Vec::new()),
        content: content.to_string(),
    });
    let app = Router::new()
        .route(
            "/chat/completions",
            axum::routing::post(blocking_auxiliary_provider_handler),
        )
        .with_state(Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("auxiliary Provider listener should bind");
    let address = listener
        .local_addr()
        .expect("auxiliary Provider listener should expose its address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (
        format!("http://{address}"),
        state,
        QueuedSseServerGuard { handle },
    )
}

async fn queued_openai_sse_handler(
    State((responses, requests)): State<QueuedSseState>,
    Json(request): Json<serde_json::Value>,
) -> Response {
    requests.lock().await.push(request);
    let response = responses.lock().await.pop_front();
    match response {
        Some(body) => (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            body,
        )
            .into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn spawn_queued_openai_sse_server(
    responses: Vec<String>,
) -> (
    String,
    Arc<Mutex<Vec<serde_json::Value>>>,
    QueuedSseServerGuard,
) {
    let responses = Arc::new(Mutex::new(VecDeque::from(responses)));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route(
            "/chat/completions",
            axum::routing::post(queued_openai_sse_handler),
        )
        .with_state((Arc::clone(&responses), Arc::clone(&requests)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("queued SSE listener should bind");
    let address = listener
        .local_addr()
        .expect("queued SSE listener should expose its address");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (
        format!("http://{address}"),
        requests,
        QueuedSseServerGuard { handle },
    )
}

fn openai_tool_call_sse(id: &str, name: &str, arguments: serde_json::Value) -> String {
    let arguments = serde_json::to_string(&arguments).expect("tool arguments should encode");
    let event = json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments}
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    format!("data: {event}\n\ndata: [DONE]\n\n")
}

fn openai_text_sse(content: &str) -> String {
    let event = json!({
        "choices": [{
            "delta": {"content": content},
            "finish_reason": "stop"
        }]
    });
    format!("data: {event}\n\ndata: [DONE]\n\n")
}

#[tokio::test]
async fn initial_session_sync_omits_group_discovery_when_groups_are_disabled() {
    let mut config = test_config();
    config.enable_groups = false;
    let state = test_app_state_with_config(config);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            MAIN_SESSION_ID.to_string(),
            test_session(MAIN_SESSION_ID, "Main", None),
        );
    }

    send_existing_session_payloads(&tx, &state, MAIN_SESSION_ID).await;

    let events = [
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
    ];
    let event_types = events
        .iter()
        .map(|payload| payload["type"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        event_types,
        [
            "session",
            "view_state",
            "todos_state",
            "history",
            "feature_status"
        ]
    );
    assert_eq!(events[4]["features"]["groups"], false);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .is_err(),
        "disabled Group discovery must not emit an additional WebSocket payload"
    );
}

#[tokio::test]
async fn handle_idle_socket_input_accepts_plan_mode_payload_without_images() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }

    let action = handle_idle_socket_input(
        r#"{"text":"inspect the runtime","plan_mode":false}"#.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(
        action,
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::Execute,
            ..
        }
    ));

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_id).expect("session should exist");
    let message = session
        .messages
        .last()
        .expect("user message should be stored");
    assert_eq!(message.role, "user");
    assert_eq!(message.content.as_deref(), Some("inspect the runtime"));
    assert!(message.images.is_none());
}

#[tokio::test]
async fn handle_idle_socket_input_rejects_agent_start_without_an_explicit_model() {
    let mut config = test_config();
    config.explicit_primary_model_configured = false;
    let state = Arc::new(test_app_state());
    state.replace_config(config);
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }

    let action = handle_idle_socket_input(
        "do not use the fallback".into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["type"], "error");
    assert!(
        event["content"]
            .as_str()
            .unwrap()
            .contains("explicit model")
    );
    let sessions = state.sessions.lock().await;
    assert_eq!(sessions[&session_id].messages.len(), 1);
}

#[tokio::test]
async fn handle_idle_socket_input_keeps_unknown_slash_commands_model_free() {
    let mut config = test_config();
    config.explicit_primary_model_configured = false;
    let state = Arc::new(test_app_state());
    state.replace_config(config);
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }

    let action = handle_idle_socket_input(
        "/bogus".into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["type"], "system");
    assert_eq!(event["content"], "Unknown command. Type /help.");
    assert!(state.active_runs.lock().await.is_empty());
    let sessions = state.sessions.lock().await;
    assert_eq!(sessions[&session_id].messages.len(), 1);
}

#[tokio::test]
async fn handle_idle_socket_input_rechecks_the_model_after_reserving_the_run() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }
    let active_runs_guard = state.active_runs.lock().await;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let task_state = Arc::clone(&state);
    let task_session_id = session_id.clone();
    let task = tokio::spawn(async move {
        let mut current_session_id = task_session_id.clone();
        handle_idle_socket_input(
            "wait for the final model snapshot".into(),
            &mut current_session_id,
            &Arc::new(Mutex::new(task_session_id)),
            1,
            &task_state,
            &tx,
            &live_tx,
            &CancellationToken::new(),
            &Arc::new(AtomicBool::new(false)),
        )
        .await
    });
    tokio::task::yield_now().await;
    let mut disabled = test_config();
    disabled.explicit_primary_model_configured = false;
    state.replace_config(disabled);
    drop(active_runs_guard);

    let action = task.await.expect("input task should complete");
    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["type"], "error");
    assert!(
        event["content"]
            .as_str()
            .unwrap()
            .contains("explicit model")
    );
    assert_eq!(state.sessions.lock().await[&session_id].messages.len(), 1);
    assert!(!state.active_runs.lock().await.contains_key(&session_id));
}

#[tokio::test]
async fn handle_idle_socket_input_releases_reservation_when_session_is_missing() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    let action = handle_idle_socket_input(
        r#"{"text":"inspect the runtime","plan_mode":false}"#.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["type"], "error");
    assert_eq!(event["content"], "Current session not found.");

    let active_runs = state.active_runs.lock().await;
    assert!(
        !active_runs.contains_key(&session_id),
        "reservation should be released when the session cannot receive the message"
    );
}

#[tokio::test]
async fn handle_idle_socket_input_does_not_start_when_the_user_message_cannot_be_saved() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "persist-failure-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let original_session = test_session(&session_id, "Persist Failure", None);
    let original_updated_at = original_session.updated_at;
    let original_message_count = original_session.messages.len();
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), original_session);
    }

    let failure_path = crate::session_store::sessions_dir().join(format!("{session_id}.json.tmp"));
    std::fs::create_dir_all(&failure_path).expect("failure sentinel directory should be created");

    let action = handle_idle_socket_input(
        "this message must be durable".into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    std::fs::remove_dir_all(&failure_path).expect("failure sentinel should be removed");
    assert!(matches!(action, IdleSocketInputAction::Continue));
    assert!(!state.active_runs.lock().await.contains_key(&session_id));
    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&session_id)
        .expect("session should remain loaded");
    assert_eq!(session.messages.len(), original_message_count);
    assert_eq!(session.updated_at, original_updated_at);
    drop(sessions);

    let storage_event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(storage_event["type"], "storage_status");
    let error_event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(error_event["type"], "error");
    assert!(
        error_event["content"]
            .as_str()
            .is_some_and(|content| content.contains("Agent run was not started"))
    );
}

#[tokio::test]
async fn handle_idle_socket_input_accepts_plan_only_payload() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }

    let action = handle_idle_socket_input(
        r#"{"text":"inspect the runtime","plan_mode":true}"#.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(
        action,
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::PlanOnly,
            ..
        }
    ));

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_id).expect("session should exist");
    let message = session
        .messages
        .last()
        .expect("user message should be stored");
    assert_eq!(message.role, "user");
    assert_eq!(message.content.as_deref(), Some("inspect the runtime"));
    assert!(message.images.is_none());
}

#[tokio::test]
async fn handle_idle_socket_input_defaults_structured_payload_to_execute_mode() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }

    let action = handle_idle_socket_input(
        r#"{"text":"inspect the runtime"}"#.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(
        action,
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::Execute,
            ..
        }
    ));
}

#[tokio::test]
async fn handle_idle_socket_input_keeps_a_resumable_terminal_plan_during_a_normal_run() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "normal-run-keeps-stopped-plan-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Stopped Plan", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_resume_later".into(),
        revision: 2,
        status: crate::plan::PlanStatus::Stopped,
        approved_at: Some(20),
        execution_attempt: 1,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);
    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let mut current_session_id = session_id.clone();

    let action = handle_idle_socket_input(
        "Answer an unrelated question first.".into(),
        &mut current_session_id,
        &Arc::new(Mutex::new(session_id.clone())),
        1,
        &state,
        &tx,
        &live_tx,
        &CancellationToken::new(),
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    let reservation = match action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::Execute,
            reservation,
            ..
        } => reservation,
        _ => panic!("a normal run should still start"),
    };
    let sessions = state.sessions.lock().await;
    let plan = sessions[&session_id]
        .pending_plan
        .as_ref()
        .expect("the stopped plan should remain resumable");
    assert_eq!(plan.id, "plan_resume_later");
    assert_eq!(plan.status, crate::plan::PlanStatus::Stopped);
    assert_eq!(
        sessions[&session_id]
            .messages
            .last()
            .and_then(|message| message.content.as_deref()),
        Some("Answer an unrelated question first.")
    );
    drop(sessions);
    release_agent_run_reservation(&state, &session_id, &reservation).await;

    let _ = std::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    );
}

#[tokio::test]
async fn handle_idle_socket_input_rejects_an_active_plan_before_image_validation() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "active-plan-before-images-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Active Plan", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_active_before_images".into(),
        revision: 3,
        status: crate::plan::PlanStatus::Ready,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let mut current_session_id = session_id.clone();
    let images = (0..11)
        .map(|index| serde_json::json!({"url": format!("https://example.com/{index}.png")}))
        .collect::<Vec<_>>();

    let action = handle_idle_socket_input(
        serde_json::json!({"text":"inspect these images","images":images}).to_string(),
        &mut current_session_id,
        &Arc::new(Mutex::new(session_id.clone())),
        1,
        &state,
        &tx,
        &live_tx,
        &CancellationToken::new(),
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["code"], "plan_already_active");
    assert_eq!(event["plan_id"], "plan_active_before_images");
    assert_eq!(state.sessions.lock().await[&session_id].messages.len(), 1);
    assert!(state.active_runs.lock().await.is_empty());
}

#[tokio::test]
async fn handle_idle_socket_input_executes_matching_pending_plan() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    let mut session = test_session(&session_id, "Main", None);
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Add plan mode execution.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: Some("1. Inspect code\n2. Apply changes\n3. Run tests".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_test".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        created_at: 10,
        ..Default::default()
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let action = handle_idle_socket_input(
        r#"{"execute_plan_id":"plan_test"}"#.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    match &action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::Execute,
            reservation,
            ..
        } => assert!(
            reservation
                .plan_action_prompt
                .as_deref()
                .is_some_and(|prompt| prompt.contains("Execute it now")),
            "approved execution needs an explicit ephemeral action prompt"
        ),
        _ => panic!("a matching approved plan should start execution"),
    }
    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_id).expect("session should exist");
    let active_plan = session
        .pending_plan
        .as_ref()
        .expect("plan should remain tracked");
    assert_eq!(active_plan.status, crate::plan::PlanStatus::Executing);
    assert_eq!(active_plan.execution_attempt, 1);
    assert_eq!(
        session.messages.len(),
        3,
        "approval must not create a fake user message"
    );
    let _ = std::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    );
}

#[tokio::test]
async fn handle_idle_socket_input_rejects_a_stale_plan_revision() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    let mut session = test_session(&session_id, "Main", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_revision".into(),
        revision: 3,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"execute","plan_id":"plan_revision","revision":2}}"#.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["type"], "error");
    assert_eq!(event["code"], "stale_plan_revision");
    assert_eq!(event["plan"]["plan_id"], "plan_revision");
    assert_eq!(event["plan"]["revision"], 3);
    assert_eq!(
        state.sessions.lock().await[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| (plan.revision, plan.status)),
        Some((3, crate::plan::PlanStatus::Ready))
    );
    assert!(!state.active_runs.lock().await.contains_key(&session_id));
}

#[tokio::test]
async fn handle_idle_socket_input_does_not_let_legacy_approval_skip_revisions() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    let mut session = test_session(&session_id, "Main", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_revised".into(),
        revision: 2,
        status: crate::plan::PlanStatus::Ready,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let action = handle_idle_socket_input(
        r#"{"execute_plan_id":"plan_revised"}"#.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["code"], "stale_plan_revision");
    assert_eq!(event["plan"]["revision"], 2);
    assert_eq!(
        state.sessions.lock().await[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| (plan.revision, plan.status, plan.execution_attempt)),
        Some((2, crate::plan::PlanStatus::Ready, 0))
    );
    assert!(state.active_runs.lock().await.is_empty());
}

#[tokio::test]
async fn handle_idle_socket_input_does_not_resume_an_unapproved_stopped_plan() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "stopped-planning-plan-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Stopped Planning Plan", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_stopped_while_planning".into(),
        revision: 1,
        status: crate::plan::PlanStatus::Stopped,
        approved_at: None,
        execution_attempt: 0,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let mut current_session_id = session_id.clone();

    let action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"resume","plan_id":"plan_stopped_while_planning","revision":1}}"#.into(),
        &mut current_session_id,
        &Arc::new(Mutex::new(session_id.clone())),
        1,
        &state,
        &tx,
        &live_tx,
        &CancellationToken::new(),
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["code"], "plan_not_ready");
    assert_eq!(
        state.sessions.lock().await[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Stopped)
    );
    assert!(!state.active_runs.lock().await.contains_key(&session_id));
}

#[tokio::test]
async fn handle_idle_socket_input_resumes_a_stopped_approved_execution() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "stopped-approved-plan-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Stopped Approved Plan", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_stopped_after_approval".into(),
        revision: 2,
        status: crate::plan::PlanStatus::Stopped,
        approved_at: Some(20),
        execution_attempt: 1,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);
    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let mut current_session_id = session_id.clone();

    let action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"resume","plan_id":"plan_stopped_after_approval","revision":2}}"#.into(),
        &mut current_session_id,
        &Arc::new(Mutex::new(session_id.clone())),
        1,
        &state,
        &tx,
        &live_tx,
        &CancellationToken::new(),
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    let reservation = match action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::Execute,
            reservation,
            ..
        } => reservation,
        _ => panic!("an approved stopped execution should resume"),
    };
    let sessions = state.sessions.lock().await;
    let plan = sessions[&session_id]
        .pending_plan
        .as_ref()
        .expect("plan should remain available");
    assert_eq!(plan.status, crate::plan::PlanStatus::Executing);
    assert_eq!(plan.execution_attempt, 2);
    assert_eq!(plan.approved_at, Some(20));
    drop(sessions);
    release_agent_run_reservation(&state, &session_id, &reservation).await;

    let _ = std::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    );
}

#[tokio::test]
async fn plan_feedback_is_durable_without_becoming_a_transcript_message() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "plan-feedback-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Plan Feedback", None);
    session.version = crate::SESSION_VERSION;
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_feedback".into(),
        revision: 3,
        status: crate::plan::PlanStatus::NeedsInput,
        artifact: crate::plan::PlanArtifact {
            title: "Choose storage".into(),
            goal: "Choose a storage engine".into(),
            questions: vec![crate::plan::PlanQuestion {
                id: "storage".into(),
                prompt: "Which storage engine?".into(),
                options: Vec::new(),
            }],
            ..Default::default()
        },
        created_at: 10,
        ..Default::default()
    });
    let original_message_count = session.messages.len();
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);
    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let mut current_session_id = session_id.clone();

    let action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"feedback","plan_id":"plan_feedback","revision":3,"answers":{"storage":"SQLite"}}}"#.into(),
        &mut current_session_id,
        &Arc::new(Mutex::new(session_id.clone())),
        1,
        &state,
        &tx,
        &live_tx,
        &CancellationToken::new(),
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    let reservation = match action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::PlanOnly,
            reservation,
            ..
        } => reservation,
        _ => panic!("valid feedback should start a PlanOnly run"),
    };
    assert_eq!(
        reservation.plan_action_prompt.as_deref(),
        Some("Answers to the blocking plan questions:\n- Which storage engine?: SQLite")
    );
    let sessions = state.sessions.lock().await;
    assert_eq!(sessions[&session_id].messages.len(), original_message_count);
    assert_eq!(
        sessions[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Planning)
    );
    assert_eq!(
        sessions[&session_id]
            .pending_plan
            .as_ref()
            .and_then(|plan| plan.pending_feedback.as_deref()),
        Some("Answers to the blocking plan questions:\n- Which storage engine?: SQLite")
    );
    drop(sessions);
    let saved = crate::session_store::load_session_from_disk(&session_id)
        .expect("feedback should be recoverable from durable Session state");
    assert_eq!(
        saved
            .pending_plan
            .as_ref()
            .and_then(|plan| plan.pending_feedback.as_deref()),
        Some("Answers to the blocking plan questions:\n- Which storage engine?: SQLite")
    );
    release_agent_run_reservation(&state, &session_id, &reservation).await;

    let _ = std::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    );
}

#[tokio::test]
async fn plan_feedback_terminal_outcome_uses_the_registered_replacement_revision() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "plan-feedback-terminal-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("plan-feedback-terminal");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let mut session = test_session(&session_id, "Plan Feedback Terminal", None);
    session.version = crate::SESSION_VERSION;
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Plan the storage migration.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(10),
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: Some("Which storage engine should the plan use?".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(11),
    });
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_feedback_terminal".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        revision: 3,
        status: crate::plan::PlanStatus::NeedsInput,
        artifact: crate::plan::PlanArtifact {
            title: "Choose storage".into(),
            goal: "Choose a storage engine".into(),
            questions: vec![crate::plan::PlanQuestion {
                id: "storage".into(),
                prompt: "Which storage engine?".into(),
                options: Vec::new(),
            }],
            ..Default::default()
        },
        created_at: 10,
        updated_at: 11,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let (inbound_tx, _inbound_rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let mut current_session_id = session_id.clone();
    let cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"feedback","plan_id":"plan_feedback_terminal","revision":3,"answers":{"storage":"SQLite"}}}"#.into(),
        &mut current_session_id,
        &current_session_ref,
        19,
        &state,
        &inbound_tx,
        &live_tx,
        &cancel,
        &stop_requested,
    )
    .await;
    let reservation = match action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::PlanOnly,
            reservation,
            ..
        } => reservation,
        _ => panic!("valid feedback should reserve a PlanOnly run"),
    };
    {
        let mut sessions = state.sessions.lock().await;
        let session = sessions
            .get_mut(&session_id)
            .expect("feedback Session should remain loaded");
        session.messages.push(ChatMessage {
            role: "assistant".into(),
            content: Some("I incorporated the storage answer.".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "submit-feedback-revision".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: crate::plan::TOOL_NAME_SUBMIT_PLAN.into(),
                    arguments: "{}".into(),
                },
            }]),
            tool_call_id: None,
            timestamp: Some(12),
        });
        session.messages.push(ChatMessage {
            role: "tool".into(),
            content: Some("Plan accepted.".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("submit-feedback-revision".into()),
            timestamp: Some(13),
        });
    }

    let run_cancel = CancellationToken::new();
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;
    phase_state.plan_action_prompt = reservation.plan_action_prompt.clone();
    phase_state.plan_submission = Some(
        crate::plan::validate_submission_json(
            r#"{
                "state":"ready",
                "title":"SQLite migration",
                "goal":"Migrate the Session store to SQLite",
                "steps":[{"id":"migrate","title":"Implement the SQLite migration"}],
                "acceptance_criteria":["The workspace remains available after migration."],
                "completion_checks":[{"id":"workspace","step_id":"migrate","covers":[{"section":"acceptance_criteria","index":0}],"kind":"workspace_path","path":".","expected_path_type":"directory"}]
            }"#,
        )
        .expect("feedback replacement submission should validate"),
    );
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    assert!(matches!(
        run_finish_phase(&ctx, &mut phase_state).await,
        AgentPhaseControl::Break
    ));
    assert!(!phase_state.run_failed);
    let (terminal_snapshot, outcome) = take_terminal_persistence_record(&session_id)
        .await
        .expect("feedback terminal transaction should be captured");
    let final_plan = terminal_snapshot
        .pending_plan
        .as_ref()
        .expect("replacement plan should be committed");
    assert_eq!(final_plan.id, "plan_feedback_terminal");
    assert_eq!(final_plan.revision, 4);
    assert_eq!(final_plan.status, crate::plan::PlanStatus::Ready);
    assert_eq!(outcome.plan_id.as_deref(), Some("plan_feedback_terminal"));
    assert_eq!(outcome.plan_revision, Some(4));
    assert!(std::iter::from_fn(|| live_rx.try_recv().ok()).any(|event| {
        event["type"] == "plan_ready"
            && event["plan_id"] == "plan_feedback_terminal"
            && event["revision"] == 4
    }));

    release_agent_run_reservation(&state, &session_id, &reservation).await;
}

#[test]
fn plan_action_context_is_an_ephemeral_user_message() {
    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: Some("System policy".to_string()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    append_plan_action_user_context(&mut messages, Some("Use SQLite"));

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[0].content.as_deref(), Some("System policy"));
    assert_eq!(messages[1].role, "user");
    assert_eq!(
        messages[1].content.as_deref(),
        Some("Current plan action from the user:\n\nUse SQLite")
    );
    assert!(messages[1].timestamp.is_none());
}

#[test]
fn submit_plan_escape_errors_return_actionable_retry_guidance() {
    let guidance =
        submit_plan_retry_guidance("invalid plan JSON: invalid escape at line 1 column 868");
    assert!(guidance.contains("workspace-relative"));
    assert!(guidance.contains("Windows path"));
    assert!(guidance.contains("raw newlines"));
    assert!(submit_plan_retry_guidance("steps must not be empty").is_empty());
}

#[test]
fn internal_plan_tool_results_do_not_trigger_an_auxiliary_state_digest() {
    let internal_error = agent::ToolResultEntry {
        id: "submit-plan".into(),
        name: crate::plan::TOOL_NAME_SUBMIT_PLAN.into(),
        result: "submit_plan validation failed: invalid escape".into(),
        duration_ms: 0,
        is_error: true,
        call_summary: None,
        trace: None,
    };
    assert!(agent::should_trigger_state_digest(std::slice::from_ref(
        &internal_error
    )));
    assert!(!should_run_working_state_digest(std::slice::from_ref(
        &internal_error
    )));

    let workspace_error = agent::ToolResultEntry {
        id: "read-file".into(),
        name: crate::tools::TOOL_NAME_READ_FILE.into(),
        result: "read_file error: unavailable".into(),
        duration_ms: 1,
        is_error: true,
        call_summary: None,
        trace: None,
    };
    assert!(should_run_working_state_digest(std::slice::from_ref(
        &workspace_error
    )));
    assert!(should_run_working_state_digest(&[
        workspace_error,
        internal_error
    ]));
}

#[tokio::test]
async fn handle_plan_action_rechecks_status_after_reserving_the_run() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "plan-status-race-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Plan Status Race", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_status_race".into(),
        revision: 1,
        status: crate::plan::PlanStatus::Ready,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let persist_gate = crate::session_store::session_persist_gate(&session_id);
    let persist_guard = persist_gate.lock().await;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let task_state = Arc::clone(&state);
    let task_session_id = session_id.clone();
    let task = tokio::spawn(async move {
        let mut current_session_id = task_session_id.clone();
        handle_idle_socket_input(
            r#"{"plan_action":{"action":"execute","plan_id":"plan_status_race","revision":1}}"#
                .into(),
            &mut current_session_id,
            &Arc::new(Mutex::new(task_session_id)),
            7,
            &task_state,
            &tx,
            &live_tx,
            &CancellationToken::new(),
            &Arc::new(AtomicBool::new(false)),
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if state.active_runs.lock().await.contains_key(&session_id) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("action should reserve the run before waiting for persistence");
    {
        let mut sessions = state.sessions.lock().await;
        sessions
            .get_mut(&session_id)
            .and_then(|session| session.pending_plan.as_mut())
            .expect("plan should exist")
            .status = crate::plan::PlanStatus::Completed;
    }
    drop(persist_guard);

    let action = task.await.expect("plan action should finish");
    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["code"], "plan_not_ready");
    assert_eq!(
        state.sessions.lock().await[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Completed)
    );
    assert!(!state.active_runs.lock().await.contains_key(&session_id));
}

#[tokio::test]
async fn terminal_plans_reject_refresh_and_discard_actions() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "plan-terminal-actions-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Plan Terminal Actions", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_terminal_actions".into(),
        revision: 1,
        status: crate::plan::PlanStatus::Completed,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);

    for status in [
        crate::plan::PlanStatus::Completed,
        crate::plan::PlanStatus::Discarded,
    ] {
        {
            let mut sessions = state.sessions.lock().await;
            sessions
                .get_mut(&session_id)
                .and_then(|session| session.pending_plan.as_mut())
                .expect("plan should exist")
                .status = status;
        }
        for action in [
            socket_input::PlanActionKind::Refresh,
            socket_input::PlanActionKind::Discard,
        ] {
            let result = socket_input::handle_plan_action(
                socket_input::PlanActionPayload {
                    action,
                    plan_id: "plan_terminal_actions".into(),
                    revision: 1,
                    text: None,
                    answers: Default::default(),
                    allow_stale: false,
                    stale_confirmation_token: None,
                },
                &session_id,
                7,
                &state,
                &tx,
                &CancellationToken::new(),
                &Arc::new(AtomicBool::new(false)),
            )
            .await;

            assert!(matches!(result, IdleSocketInputAction::Continue));
            let event = recv_json_with_timeout(&mut rx).await;
            assert_eq!(event["code"], "plan_not_ready");
            assert_eq!(
                state.sessions.lock().await[&session_id]
                    .pending_plan
                    .as_ref()
                    .map(|plan| plan.status),
                Some(status)
            );
            assert!(!state.active_runs.lock().await.contains_key(&session_id));
        }
    }
}

#[tokio::test]
async fn discard_plan_cannot_race_an_active_agent_run() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "plan-discard-race-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Plan Discard Race", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_discard_race".into(),
        revision: 1,
        status: crate::plan::PlanStatus::Ready,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);
    let active_cancel = CancellationToken::new();
    let active_stop = Arc::new(AtomicBool::new(false));
    let active_reservation =
        try_reserve_agent_run(&state, &session_id, 99, &active_cancel, &active_stop)
            .await
            .expect("fixture run should reserve");
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let action = socket_input::handle_plan_action(
        socket_input::PlanActionPayload {
            action: socket_input::PlanActionKind::Discard,
            plan_id: "plan_discard_race".into(),
            revision: 1,
            text: None,
            answers: Default::default(),
            allow_stale: false,
            stale_confirmation_token: None,
        },
        &session_id,
        7,
        &state,
        &tx,
        &CancellationToken::new(),
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["code"], "plan_already_active");
    assert_eq!(
        state.sessions.lock().await[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Ready)
    );
    release_agent_run_reservation(&state, &session_id, &active_reservation).await;
}

#[tokio::test]
async fn refresh_plan_preserves_current_revision_evidence_until_replacement() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "plan-refresh-evidence-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("refresh-evidence");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    std::fs::write(workspace.join("evidence.txt"), "current").expect("fixture should be written");
    let evidence = crate::plan::capture_tool_evidence(
        crate::tools::TOOL_NAME_READ_FILE,
        r#"{"path":"evidence.txt"}"#,
        &workspace,
    );
    let mut session = test_session(&session_id, "Plan Refresh Evidence", None);
    session.workspace = workspace.clone();
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_refresh_evidence".into(),
        revision: 2,
        status: crate::plan::PlanStatus::Ready,
        evidence: evidence.clone(),
        evidence_truncated: true,
        created_at: 10,
        ..Default::default()
    });
    let original_message_count = session.messages.len();
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);
    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let mut current_session_id = session_id.clone();

    let action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"refresh","plan_id":"plan_refresh_evidence","revision":2}}"#
            .into(),
        &mut current_session_id,
        &Arc::new(Mutex::new(session_id.clone())),
        7,
        &state,
        &tx,
        &live_tx,
        &CancellationToken::new(),
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    let reservation = match action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::PlanOnly,
            reservation,
            ..
        } => reservation,
        _ => panic!("refresh should start a PlanOnly run"),
    };
    assert!(reservation.reset_plan_evidence);
    assert_eq!(
        reservation.plan_action_prompt.as_deref(),
        Some("Refresh this plan against the current workspace state and submit a new revision.")
    );
    let sessions = state.sessions.lock().await;
    let plan = sessions[&session_id]
        .pending_plan
        .as_ref()
        .expect("plan should remain available");
    assert_eq!(plan.status, crate::plan::PlanStatus::Planning);
    assert_eq!(plan.evidence, evidence);
    assert!(plan.evidence_truncated);
    assert_eq!(
        sessions[&session_id].messages.len(),
        original_message_count,
        "refresh controls must not create a user transcript message"
    );
    drop(sessions);
    release_agent_run_reservation(&state, &session_id, &reservation).await;

    let _ = std::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    );
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn handle_idle_socket_input_reports_changed_plan_evidence_before_execution() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "plan-stale-confirmation-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let workspace = temp_workspace("plan-stale-evidence");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    std::fs::write(workspace.join("evidence.txt"), "before")
        .expect("evidence file should be written");

    let evidence = crate::plan::capture_tool_evidence(
        crate::tools::TOOL_NAME_READ_FILE,
        r#"{"path":"evidence.txt"}"#,
        &workspace,
    );
    assert_eq!(evidence.len(), 1);
    std::fs::write(workspace.join("evidence.txt"), "after")
        .expect("evidence file should be changed");

    let mut session = test_session(&session_id, "Plan Stale Confirmation", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_evidence".into(),
        revision: 2,
        evidence,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"execute","plan_id":"plan_evidence","revision":2,"allow_stale":true}}"#.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["type"], "plan_stale");
    assert_eq!(event["code"], "plan_stale");
    assert_eq!(event["plan_id"], "plan_evidence");
    assert_eq!(event["revision"], 2);
    assert_eq!(event["paths"], serde_json::json!(["evidence.txt"]));
    let first_confirmation_token = event["confirmation_token"]
        .as_str()
        .expect("stale evidence should include a confirmation token")
        .to_string();
    assert!(!first_confirmation_token.is_empty());
    assert_eq!(
        state.sessions.lock().await[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Ready)
    );
    assert!(!state.active_runs.lock().await.contains_key(&session_id));

    std::fs::write(workspace.join("evidence.txt"), "changed again")
        .expect("evidence file should change again");
    let action = handle_idle_socket_input(
        serde_json::json!({
            "plan_action": {
                "action": "execute",
                "plan_id": "plan_evidence",
                "revision": 2,
                "allow_stale": true,
                "stale_confirmation_token": first_confirmation_token,
            }
        })
        .to_string(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["type"], "plan_stale");
    let second_confirmation_token = event["confirmation_token"]
        .as_str()
        .expect("changed evidence should replace the confirmation token")
        .to_string();
    assert_ne!(second_confirmation_token, first_confirmation_token);

    let action = handle_idle_socket_input(
        serde_json::json!({
            "plan_action": {
                "action": "execute",
                "plan_id": "plan_evidence",
                "revision": 2,
                "allow_stale": true,
                "stale_confirmation_token": second_confirmation_token,
            }
        })
        .to_string(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    let reservation = match action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::Execute,
            reservation,
            ..
        } => reservation,
        _ => panic!("the current evidence confirmation should start execution"),
    };
    let plan = state.sessions.lock().await[&session_id]
        .pending_plan
        .clone()
        .expect("plan should remain available");
    assert_eq!(plan.status, crate::plan::PlanStatus::Executing);
    assert_eq!(plan.stale_override_paths, vec!["evidence.txt"]);
    assert!(
        reservation
            .plan_action_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("Execute it now"))
    );
    release_agent_run_reservation(&state, &session_id, &reservation).await;

    let _ = std::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    );
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn refreshed_stale_override_runs_tools_and_completes_only_reported_steps() {
    let responses = vec![
        openai_tool_call_sse(
            "refresh-read-spec",
            crate::tools::TOOL_NAME_READ_FILE,
            json!({"path":"spec.txt"}),
        ),
        openai_tool_call_sse(
            "refresh-submit-plan",
            crate::plan::TOOL_NAME_SUBMIT_PLAN,
            json!({
                "state": "ready",
                "title": "Refreshed result plan",
                "goal": "Create result.txt from the refreshed specification",
                "summary": "Use the latest spec.txt evidence before writing the result.",
                "steps": [{
                    "id": "write-result",
                    "title": "Write and verify result.txt",
                    "description": "Create the requested file in the workspace.",
                    "affected_areas": ["result.txt"]
                }],
                "verification": ["Read result.txt and verify its content."],
                "acceptance_criteria": ["result.txt exists with the requested content."],
                "completion_checks": [{
                    "id": "result-file",
                    "step_id": "write-result",
                    "covers": [
                        {"section": "verification", "index": 0},
                        {"section": "acceptance_criteria", "index": 0}
                    ],
                    "kind": "workspace_path",
                    "path": "result.txt",
                    "expected_path_type": "file",
                    "exact_content": "executed revision 3",
                    "size_bytes": 19
                }]
            }),
        ),
        openai_tool_call_sse(
            "write-result",
            crate::tools::TOOL_NAME_WRITE_FILE,
            json!({"path":"result.txt", "content":"executed revision 3"}),
        ),
        openai_tool_call_sse(
            "complete-and-adapt",
            crate::plan::TOOL_NAME_UPDATE_PLAN,
            json!({
                "base_revision": 3,
                "updates": [{
                    "id": "write-result",
                    "status": "completed",
                    "note": "Created and verified result.txt"
                }],
                "append_steps": [{
                    "id": "adapt-readback",
                    "title": "Read the final file once more",
                    "note": "Add a harmless final-state check",
                    "deviation_reason": "A final readback strengthens verification without changing the approved output"
                }]
            }),
        ),
        openai_tool_call_sse(
            "readback-result",
            crate::tools::TOOL_NAME_READ_FILE,
            json!({"path":"result.txt"}),
        ),
        openai_tool_call_sse(
            "complete-adaptation",
            crate::plan::TOOL_NAME_UPDATE_PLAN,
            json!({
                "base_revision": 3,
                "updates": [{
                    "id": "adapt-readback",
                    "status": "completed",
                    "note": "Readback preserved the approved exact content"
                }]
            }),
        ),
        openai_text_sse("The approved revision was executed and verified."),
    ];
    let (api_base, requests, _server) = spawn_queued_openai_sse_server(responses).await;
    let mut config = test_config();
    config.api_base = api_base;
    let state = Arc::new(test_app_state_with_config(config));
    let session_id = format!(
        "refreshed-stale-execution-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("refreshed-stale-execution");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);
    std::fs::write(workspace.join("spec.txt"), "revision two")
        .expect("initial evidence should be written");
    let revision_two_evidence = crate::plan::capture_tool_evidence(
        crate::tools::TOOL_NAME_READ_FILE,
        r#"{"path":"spec.txt"}"#,
        &workspace,
    );
    let artifact = crate::plan::PlanArtifact {
        title: "Create the result".into(),
        goal: "Create result.txt from the approved specification".into(),
        steps: vec![crate::plan::PlanStep {
            id: "write-result".into(),
            title: "Write and verify result.txt".into(),
            description: "Create the requested file in the workspace.".into(),
            affected_areas: vec!["result.txt".into()],
        }],
        verification: vec!["Read result.txt and verify its content.".into()],
        acceptance_criteria: vec!["result.txt exists with the requested content.".into()],
        ..Default::default()
    };
    let progress = vec![crate::plan::PlanProgressStep {
        id: "write-result".into(),
        title: "Write and verify result.txt".into(),
        ..Default::default()
    }];
    let mut session = test_session(&session_id, "Refreshed stale execution", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.workspace_kind = crate::SessionWorkspaceKind::Directory;
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Create result.txt according to spec.txt.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: Some("Plan revision two".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_refreshed_stale".into(),
        revision: 2,
        status: crate::plan::PlanStatus::Ready,
        artifact: artifact.clone(),
        progress: progress.clone(),
        evidence: revision_two_evidence,
        created_at: 10,
        updated_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    std::fs::write(workspace.join("spec.txt"), "revision three")
        .expect("the specification should change before the refresh run");

    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let (refresh_tx, _refresh_rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let mut current_session_id = session_id.clone();
    let refresh_action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"refresh","plan_id":"plan_refreshed_stale","revision":2}}"#
            .into(),
        &mut current_session_id,
        &current_session_ref,
        7,
        &state,
        &refresh_tx,
        &live_tx,
        &cancel,
        &stop_requested,
    )
    .await;
    let (refresh_reservation, refresh_model_snapshot) = match refresh_action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::PlanOnly,
            reservation,
            model_snapshot,
            ..
        } => (reservation, model_snapshot),
        _ => panic!("refresh should start a PlanOnly revision run"),
    };
    assert!(refresh_reservation.reset_plan_evidence);
    assert!(
        refresh_reservation
            .plan_action_prompt
            .as_deref()
            .is_some_and(|prompt| prompt.contains("Refresh this plan"))
    );
    let (_refresh_inbound_tx, mut refresh_inbound_rx) = tokio::sync::mpsc::channel::<String>(4);
    let refresh_outcome = run_agent_session(
        &state,
        &session_id,
        7,
        &cancel,
        &live_tx,
        &mut refresh_inbound_rx,
        &stop_requested,
        AgentRunMode::PlanOnly,
        Some(refresh_reservation),
        Some(refresh_model_snapshot),
    )
    .await;
    assert!(!refresh_outcome.run_failed);
    let (refresh_terminal_snapshot, refresh_terminal_outcome) =
        take_terminal_persistence_record(&session_id)
            .await
            .expect("refresh terminal producer should freeze its Session snapshot");
    assert_eq!(
        refresh_terminal_snapshot
            .pending_plan
            .as_ref()
            .map(|plan| plan.revision),
        Some(3)
    );
    assert_eq!(
        refresh_terminal_outcome.plan_id.as_deref(),
        Some("plan_refreshed_stale")
    );
    assert_eq!(refresh_terminal_outcome.plan_revision, Some(3));

    let refreshed_plan = state.sessions.lock().await[&session_id]
        .pending_plan
        .clone()
        .expect("the Provider-submitted refresh should register a plan");
    assert_eq!(refreshed_plan.id, "plan_refreshed_stale");
    assert_eq!(refreshed_plan.revision, 3);
    assert_eq!(refreshed_plan.status, crate::plan::PlanStatus::Ready);
    assert_eq!(refreshed_plan.artifact.title, "Refreshed result plan");
    assert_eq!(refreshed_plan.progress.len(), 1);
    assert_eq!(refreshed_plan.evidence.len(), 1);
    assert!(crate::plan::verify_evidence(&workspace, &refreshed_plan.evidence).is_empty());
    let persisted: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            crate::session_store::sessions_dir().join(format!("{session_id}.json")),
        )
        .expect("the refreshed Session snapshot should be persisted"),
    )
    .expect("the refreshed Session snapshot should remain valid JSON");
    assert_eq!(persisted["pending_plan"]["revision"], 3);
    assert_eq!(persisted["pending_plan"]["status"], "ready");
    assert_eq!(
        persisted["pending_plan"]["artifact"]["title"],
        "Refreshed result plan"
    );

    {
        let requests = requests.lock().await;
        assert_eq!(requests.len(), 2, "refresh should use two model cycles");
        let refresh_messages = requests[0]["messages"]
            .as_array()
            .expect("refresh request should contain messages");
        assert!(refresh_messages.iter().any(|message| {
            message["role"] == "user"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("Refresh this plan"))
        }));
        assert!(requests[0]["tools"].as_array().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool["function"]["name"] == crate::plan::TOOL_NAME_SUBMIT_PLAN)
        }));
    }

    std::fs::write(workspace.join("spec.txt"), "stale after revision three")
        .expect("the refreshed evidence should become stale");
    assert_eq!(
        crate::plan::verify_evidence(&workspace, &refreshed_plan.evidence),
        vec!["spec.txt"]
    );

    let (stale_tx, mut stale_rx) = tokio::sync::mpsc::channel::<String>(8);
    let stale_action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"execute","plan_id":"plan_refreshed_stale","revision":3}}"#
            .into(),
        &mut current_session_id,
        &current_session_ref,
        7,
        &state,
        &stale_tx,
        &live_tx,
        &cancel,
        &stop_requested,
    )
    .await;
    assert!(matches!(stale_action, IdleSocketInputAction::Continue));
    let stale_event = recv_json_with_timeout(&mut stale_rx).await;
    assert_eq!(stale_event["type"], "plan_stale");
    assert_eq!(stale_event["revision"], 3);
    let confirmation_token = stale_event["confirmation_token"]
        .as_str()
        .expect("stale revision should provide a confirmation token")
        .to_string();

    let (approval_tx, _approval_rx) = tokio::sync::mpsc::channel::<String>(8);
    let approval_action = handle_idle_socket_input(
        json!({
            "plan_action": {
                "action": "execute",
                "plan_id": "plan_refreshed_stale",
                "revision": 3,
                "allow_stale": true,
                "stale_confirmation_token": confirmation_token,
            }
        })
        .to_string(),
        &mut current_session_id,
        &current_session_ref,
        7,
        &state,
        &approval_tx,
        &live_tx,
        &cancel,
        &stop_requested,
    )
    .await;
    let (reservation, model_snapshot) = match approval_action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::Execute,
            reservation,
            model_snapshot,
        } => (reservation, model_snapshot),
        _ => panic!("confirmed current stale snapshot should start execution"),
    };

    let (_inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<String>(4);
    let outcome = run_agent_session(
        &state,
        &session_id,
        7,
        &cancel,
        &live_tx,
        &mut inbound_rx,
        &stop_requested,
        AgentRunMode::Execute,
        Some(reservation),
        Some(model_snapshot),
    )
    .await;

    if outcome.run_failed {
        let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
        panic!("refreshed stale execution failed unexpectedly: {events:?}");
    }
    assert_eq!(
        std::fs::read_to_string(workspace.join("result.txt"))
            .expect("approved execution should create its target artifact"),
        "executed revision 3"
    );
    let plan = state.sessions.lock().await[&session_id]
        .pending_plan
        .clone()
        .expect("completed plan should remain visible");
    assert_eq!(plan.revision, 3);
    assert_eq!(plan.status, crate::plan::PlanStatus::Completed);
    assert_eq!(plan.unfinished_step_count(), 0);
    assert_eq!(plan.progress.len(), 2);
    assert_eq!(
        plan.progress[0].status,
        crate::plan::PlanStepStatus::Completed
    );
    assert_eq!(plan.progress[1].id, "adapt-readback");
    assert_eq!(
        plan.progress[1].status,
        crate::plan::PlanStepStatus::Completed
    );
    assert!(plan.stale_override_confirmed_at.is_some());

    let requests = requests.lock().await;
    assert_eq!(
        requests.len(),
        7,
        "refresh and adaptive execution should use seven cycles"
    );
    let first_messages = requests[2]["messages"]
        .as_array()
        .expect("OpenAI request should contain messages");
    assert!(first_messages.iter().any(|message| {
        message["role"] == "system"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("## Approved Execution Plan"))
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("Revision: 3"))
    }));
    assert!(first_messages.iter().any(|message| {
        message["role"] == "user"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("Execute it now"))
    }));
    drop(requests);
    let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| {
            event["type"] == "plan_state" && event["plan"]["status"] == "completed"
        })
    );
    assert!(
        !events.iter().any(|event| {
            event["type"] == "error" && event["code"] == "plan_execution_incomplete"
        })
    );
}

#[tokio::test]
async fn stale_override_cannot_complete_against_a_reinterpreted_refresh_contract() {
    let responses = vec![
        openai_tool_call_sse(
            "refresh-v2-read-spec",
            crate::tools::TOOL_NAME_READ_FILE,
            json!({"path":"spec.txt"}),
        ),
        openai_tool_call_sse(
            "refresh-v2-submit-plan",
            crate::plan::TOOL_NAME_SUBMIT_PLAN,
            json!({
                "state": "ready",
                "title": "Create the exact revision-two result",
                "goal": "Create result.txt with exactly ROUND2-V2-REFRESH and no trailing newline.",
                "steps": [
                    {
                        "id": "step1-create-result",
                        "title": "Write the approved revision-two content",
                        "description": "Write exactly ROUND2-V2-REFRESH to result.txt.",
                        "affected_areas": ["result.txt"]
                    },
                    {
                        "id": "step2-verify-result",
                        "title": "Verify the approved exact bytes",
                        "description": "Verify the final file is exactly 17 bytes and has no trailing newline.",
                        "affected_areas": ["result.txt"]
                    },
                    {
                        "id": "step3-report-progress",
                        "title": "Report the immutable revision progress"
                    }
                ],
                "assumptions": ["Use the current spec.txt value when execution begins."],
                "verification": ["result.txt is exactly 17 bytes with content ROUND2-V2-REFRESH."],
                "acceptance_criteria": ["result.txt contains exactly ROUND2-V2-REFRESH without a trailing newline."],
                "completion_checks": [{
                    "id": "approved-result-bytes",
                    "step_id": "step2-verify-result",
                    "covers": [
                        {"section": "verification", "index": 0},
                        {"section": "acceptance_criteria", "index": 0}
                    ],
                    "kind": "workspace_path",
                    "path": "result.txt",
                    "expected_path_type": "file",
                    "exact_content": "ROUND2-V2-REFRESH",
                    "size_bytes": 17
                }]
            }),
        ),
        openai_tool_call_sse(
            "write-approved-v2",
            crate::tools::TOOL_NAME_WRITE_FILE,
            json!({"path":"result.txt", "content":"ROUND2-V2-REFRESH"}),
        ),
        openai_tool_call_sse(
            "read-stale-v3",
            crate::tools::TOOL_NAME_READ_FILE,
            json!({"path":"spec.txt"}),
        ),
        openai_tool_call_sse(
            "append-v3-adaptation",
            crate::plan::TOOL_NAME_UPDATE_PLAN,
            json!({
                "base_revision": 2,
                "updates": [{
                    "id": "step1-create-result",
                    "status": "completed",
                    "note": "Wrote the approved ROUND2-V2-REFRESH value first."
                }],
                "append_steps": [{
                    "id": "step4-adapt-v3-override",
                    "title": "Replace the approved output with the new spec value",
                    "note": "Treat the stale input as a refreshed requirement.",
                    "deviation_reason": "spec.txt now says ROUND2-V3-OVERRIDE, so reinterpret the broad current-value assumption."
                }]
            }),
        ),
        openai_tool_call_sse(
            "rewrite-unapproved-v3",
            crate::tools::TOOL_NAME_WRITE_FILE,
            json!({"path":"result.txt", "content":"ROUND2-V3-OVERRIDE"}),
        ),
        openai_tool_call_sse(
            "claim-v3-complete",
            crate::plan::TOOL_NAME_UPDATE_PLAN,
            json!({
                "base_revision": 2,
                "updates": [
                    {
                        "id": "step2-verify-result",
                        "status": "completed",
                        "note": "Verified the reinterpreted 18-byte V3 output."
                    },
                    {
                        "id": "step3-report-progress",
                        "status": "completed",
                        "note": "All original and adaptive work is reported complete."
                    },
                    {
                        "id": "step4-adapt-v3-override",
                        "status": "completed",
                        "note": "Replaced result.txt with ROUND2-V3-OVERRIDE."
                    }
                ]
            }),
        ),
        openai_text_sse("The current specification was applied and every step is complete."),
    ];
    let (api_base, requests, _server) = spawn_queued_openai_sse_server(responses).await;
    let mut config = test_config();
    config.api_base = api_base;
    let state = Arc::new(test_app_state_with_config(config));
    let session_id = format!(
        "stale-contract-rejection-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("stale-contract-rejection");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);
    std::fs::write(workspace.join("spec.txt"), "ROUND2-V1")
        .expect("revision-one evidence should be written");
    let revision_one_evidence = crate::plan::capture_tool_evidence(
        crate::tools::TOOL_NAME_READ_FILE,
        r#"{"path":"spec.txt"}"#,
        &workspace,
    );
    let mut session = test_session(&session_id, "Stale contract rejection", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.workspace_kind = crate::SessionWorkspaceKind::Directory;
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Create result.txt according to spec.txt.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: Some("Plan revision one".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_stale_contract".into(),
        revision: 1,
        status: crate::plan::PlanStatus::Ready,
        artifact: crate::plan::PlanArtifact {
            title: "Create the initial result".into(),
            goal: "Create result.txt from revision one".into(),
            steps: vec![crate::plan::PlanStep {
                id: "write-result".into(),
                title: "Write result.txt".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        progress: vec![crate::plan::PlanProgressStep {
            id: "write-result".into(),
            title: "Write result.txt".into(),
            ..Default::default()
        }],
        evidence: revision_one_evidence,
        created_at: 10,
        updated_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    std::fs::write(workspace.join("spec.txt"), "ROUND2-V2-REFRESH")
        .expect("the source should change before refresh");
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let (socket_tx, _socket_rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let mut current_session_id = session_id.clone();
    let refresh_action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"refresh","plan_id":"plan_stale_contract","revision":1}}"#
            .into(),
        &mut current_session_id,
        &current_session_ref,
        11,
        &state,
        &socket_tx,
        &live_tx,
        &cancel,
        &stop_requested,
    )
    .await;
    let (refresh_reservation, refresh_model_snapshot) = match refresh_action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::PlanOnly,
            reservation,
            model_snapshot,
            ..
        } => (reservation, model_snapshot),
        _ => panic!("refresh should start a real PlanOnly Provider run"),
    };
    let (_refresh_input_tx, mut refresh_input_rx) = tokio::sync::mpsc::channel::<String>(4);
    let refresh_outcome = run_agent_session(
        &state,
        &session_id,
        11,
        &cancel,
        &live_tx,
        &mut refresh_input_rx,
        &stop_requested,
        AgentRunMode::PlanOnly,
        Some(refresh_reservation),
        Some(refresh_model_snapshot),
    )
    .await;
    assert!(!refresh_outcome.run_failed);
    let refreshed = state.sessions.lock().await[&session_id]
        .pending_plan
        .clone()
        .expect("Provider refresh should persist revision two");
    assert_eq!(refreshed.revision, 2);
    assert_eq!(refreshed.artifact.completion_checks.len(), 1);

    std::fs::write(workspace.join("spec.txt"), "ROUND2-V3-OVERRIDE")
        .expect("revision-two evidence should become stale again");
    let (stale_tx, mut stale_rx) = tokio::sync::mpsc::channel::<String>(8);
    let stale_action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"execute","plan_id":"plan_stale_contract","revision":2}}"#
            .into(),
        &mut current_session_id,
        &current_session_ref,
        11,
        &state,
        &stale_tx,
        &live_tx,
        &cancel,
        &stop_requested,
    )
    .await;
    assert!(matches!(stale_action, IdleSocketInputAction::Continue));
    let stale_event = recv_json_with_timeout(&mut stale_rx).await;
    assert_eq!(stale_event["type"], "plan_stale");
    let confirmation_token = stale_event["confirmation_token"]
        .as_str()
        .expect("stale evidence should issue a revision-bound token")
        .to_string();

    let approval_action = handle_idle_socket_input(
        json!({
            "plan_action": {
                "action": "execute",
                "plan_id": "plan_stale_contract",
                "revision": 2,
                "allow_stale": true,
                "stale_confirmation_token": confirmation_token,
            }
        })
        .to_string(),
        &mut current_session_id,
        &current_session_ref,
        11,
        &state,
        &stale_tx,
        &live_tx,
        &cancel,
        &stop_requested,
    )
    .await;
    let (reservation, model_snapshot) = match approval_action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::Execute,
            reservation,
            model_snapshot,
        } => (reservation, model_snapshot),
        _ => panic!("the confirmed stale snapshot should start execution"),
    };
    let (_input_tx, mut input_rx) = tokio::sync::mpsc::channel::<String>(4);
    let outcome = run_agent_session(
        &state,
        &session_id,
        11,
        &cancel,
        &live_tx,
        &mut input_rx,
        &stop_requested,
        AgentRunMode::Execute,
        Some(reservation),
        Some(model_snapshot),
    )
    .await;

    assert!(outcome.run_failed);
    assert_eq!(
        std::fs::read_to_string(workspace.join("result.txt"))
            .expect("the attempted unapproved output should remain as truthful evidence"),
        "ROUND2-V3-OVERRIDE"
    );
    let session = state.sessions.lock().await[&session_id].clone();
    let plan = session
        .pending_plan
        .as_ref()
        .expect("failed contract should remain available for revision or Resume");
    assert_eq!(plan.id, "plan_stale_contract");
    assert_eq!(plan.revision, 2);
    assert_eq!(plan.status, crate::plan::PlanStatus::Failed);
    assert_eq!(plan.execution_attempt, 1);
    assert!(plan.approved_at.is_some());
    assert!(plan.finished_at.is_some());
    assert!(plan.stale_override_confirmed_at.is_some());
    assert!(plan.artifact.goal.contains("ROUND2-V2-REFRESH"));
    assert_eq!(
        plan.artifact.completion_checks[0].exact_content.as_deref(),
        Some("ROUND2-V2-REFRESH")
    );
    assert_eq!(plan.progress.len(), 4);
    assert_eq!(
        plan.progress[0].status,
        crate::plan::PlanStepStatus::Completed
    );
    assert_eq!(
        plan.progress[1].status,
        crate::plan::PlanStepStatus::Blocked
    );
    assert!(plan.progress[1].note.contains("approved-result-bytes"));
    assert_eq!(
        plan.progress[2].status,
        crate::plan::PlanStepStatus::Completed
    );
    assert_eq!(plan.progress[3].id, "step4-adapt-v3-override");
    assert_eq!(
        plan.progress[3].status,
        crate::plan::PlanStepStatus::Completed
    );
    assert!(plan.progress[3].deviation_reason.is_some());
    assert_eq!(
        session
            .messages
            .iter()
            .filter(|message| message.role == "user")
            .count(),
        1,
        "refresh, approval, and stale override must remain ephemeral control context"
    );

    let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
    let contract_error = events
        .iter()
        .find(|event| {
            event["type"] == "error" && event["code"] == "plan_completion_contract_failed"
        })
        .expect("the immutable completion gate should report its failed check");
    assert_eq!(contract_error["run_terminal"].as_bool(), Some(true));
    assert_eq!(contract_error["plan_id"], "plan_stale_contract");
    assert_eq!(contract_error["revision"], 2);
    assert_eq!(
        contract_error["checks"][0]["check_id"],
        "approved-result-bytes"
    );
    assert!(
        events
            .iter()
            .any(|event| { event["type"] == "plan_state" && event["plan"]["status"] == "failed" })
    );
    assert!(
        !events.iter().any(|event| {
            event["type"] == "plan_state" && event["plan"]["status"] == "completed"
        })
    );

    let requests = requests.lock().await;
    assert_eq!(requests.len(), 8);
    let execution_messages = requests[2]["messages"]
        .as_array()
        .expect("execution should include the approved contract");
    assert!(execution_messages.iter().any(|message| {
        message["role"] == "system"
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("ROUND2-V2-REFRESH"))
            && message["content"]
                .as_str()
                .is_some_and(|content| content.contains("does not approve a refresh"))
    }));
}

#[tokio::test]
async fn handle_idle_socket_input_requires_confirmation_for_incomplete_plan_evidence() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "plan-incomplete-evidence-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("plan-incomplete-evidence");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let mut session = test_session(&session_id, "Incomplete Plan Evidence", None);
    session.workspace = workspace.clone();
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_incomplete_evidence".into(),
        revision: 1,
        status: crate::plan::PlanStatus::Ready,
        evidence_truncated: true,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    let action = handle_idle_socket_input(
        r#"{"plan_action":{"action":"execute","plan_id":"plan_incomplete_evidence","revision":1}}"#
            .into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["type"], "plan_stale");
    assert_eq!(event["paths"], serde_json::json!([]));
    assert_eq!(event["evidence_incomplete"], true);
    let confirmation_token = event["confirmation_token"]
        .as_str()
        .expect("incomplete evidence should require a confirmation token")
        .to_string();

    let action = handle_idle_socket_input(
        serde_json::json!({
            "plan_action": {
                "action": "execute",
                "plan_id": "plan_incomplete_evidence",
                "revision": 1,
                "allow_stale": true,
                "stale_confirmation_token": confirmation_token,
            }
        })
        .to_string(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    let reservation = match action {
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::Execute,
            reservation,
            ..
        } => reservation,
        _ => panic!("explicitly confirmed incomplete evidence should start execution"),
    };
    let plan = state.sessions.lock().await[&session_id]
        .pending_plan
        .clone()
        .expect("plan should remain available");
    assert_eq!(plan.status, crate::plan::PlanStatus::Executing);
    assert!(plan.stale_override_paths.is_empty());
    assert!(plan.stale_override_confirmed_at.is_some());
    release_agent_run_reservation(&state, &session_id, &reservation).await;

    let _ = std::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    );
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn handle_idle_socket_input_restores_the_pending_plan_when_approval_cannot_be_saved() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "plan-persist-failure-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let mut session = test_session(&session_id, "Plan Persist Failure", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_persist_failure".into(),
        original_user_message_index: 0,
        assistant_plan_message_index: 0,
        created_at: 10,
        ..Default::default()
    });
    let original_updated_at = session.updated_at;
    let original_message_count = session.messages.len();
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let failure_path = crate::session_store::sessions_dir().join(format!("{session_id}.json.tmp"));
    std::fs::create_dir_all(&failure_path).expect("failure sentinel directory should be created");

    let action = handle_idle_socket_input(
        r#"{"execute_plan_id":"plan_persist_failure"}"#.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    std::fs::remove_dir_all(&failure_path).expect("failure sentinel should be removed");
    assert!(matches!(action, IdleSocketInputAction::Continue));
    assert!(!state.active_runs.lock().await.contains_key(&session_id));
    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&session_id)
        .expect("session should remain loaded");
    assert_eq!(session.messages.len(), original_message_count);
    assert_eq!(session.updated_at, original_updated_at);
    assert_eq!(
        session.pending_plan.as_ref().map(|plan| plan.id.as_str()),
        Some("plan_persist_failure")
    );
    drop(sessions);

    let storage_event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(storage_event["type"], "storage_status");
    let error_event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(error_event["type"], "error");
    assert!(
        error_event["content"]
            .as_str()
            .is_some_and(|content| content.contains("Agent run was not started"))
    );
}

#[tokio::test]
async fn handle_idle_socket_input_keeps_a_pending_plan_when_the_final_model_is_disabled() {
    let state = Arc::new(test_app_state());
    let session_id = "pending-plan-final-model".to_string();
    let mut session = test_session(&session_id, "Pending Plan", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_final_model".into(),
        original_user_message_index: 0,
        assistant_plan_message_index: 0,
        created_at: 10,
        ..Default::default()
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }
    let active_runs_guard = state.active_runs.lock().await;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let task_state = Arc::clone(&state);
    let task_session_id = session_id.clone();
    let task = tokio::spawn(async move {
        let mut current_session_id = task_session_id.clone();
        handle_idle_socket_input(
            r#"{"execute_plan_id":"plan_final_model"}"#.into(),
            &mut current_session_id,
            &Arc::new(Mutex::new(task_session_id)),
            1,
            &task_state,
            &tx,
            &live_tx,
            &CancellationToken::new(),
            &Arc::new(AtomicBool::new(false)),
        )
        .await
    });
    tokio::task::yield_now().await;
    let mut disabled = test_config();
    disabled.explicit_primary_model_configured = false;
    state.replace_config(disabled);
    drop(active_runs_guard);

    let action = task.await.expect("plan task should complete");
    assert!(matches!(action, IdleSocketInputAction::Continue));
    let event = recv_json_with_timeout(&mut rx).await;
    assert_eq!(event["type"], "error");
    assert!(
        event["content"]
            .as_str()
            .unwrap()
            .contains("explicit model")
    );
    let sessions = state.sessions.lock().await;
    assert_eq!(
        sessions[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| plan.id.as_str()),
        Some("plan_final_model")
    );
    assert_eq!(sessions[&session_id].messages.len(), 1);
    drop(sessions);
    assert!(!state.active_runs.lock().await.contains_key(&session_id));
}

#[tokio::test]
async fn handle_idle_socket_input_rejects_execute_plan_conflicts() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }

    let action = handle_idle_socket_input(
        r#"{"execute_plan_id":"plan_test","text":"also run this"}"#.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let payload = rx.recv().await.expect("error payload should be sent");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
    assert_eq!(parsed["type"].as_str(), Some("error"));
    assert_eq!(parsed["code"].as_str(), Some("invalid_plan_action"));
    assert!(
        parsed["content"]
            .as_str()
            .expect("content")
            .contains("execute_plan_id cannot be combined")
    );
}

#[tokio::test]
async fn handle_idle_socket_input_rejects_malformed_structured_json_without_clearing_plan() {
    let state = Arc::new(test_app_state());
    let session_id = "malformed-structured-json-plan".to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    let mut session = test_session(&session_id, "Malformed Structured", None);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_test".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        created_at: 10,
        ..Default::default()
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let action = handle_idle_socket_input(
        r#"{"execute_plan_id":"plan_test""#.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let payload = rx.recv().await.expect("error payload should be sent");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
    assert_eq!(parsed["type"].as_str(), Some("system"));
    assert!(
        parsed["content"]
            .as_str()
            .expect("content")
            .contains("Invalid structured message JSON")
    );
    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_id).expect("session should exist");
    assert_eq!(
        session.pending_plan.as_ref().map(|plan| plan.id.as_str()),
        Some("plan_test")
    );
    assert_eq!(session.messages.len(), 1);
}

#[tokio::test]
async fn handle_idle_socket_input_treats_non_envelope_brace_text_as_plain_message() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }

    let input = "{ let value = compute(); }";
    let action = handle_idle_socket_input(
        input.into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(
        action,
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::Execute,
            ..
        }
    ));
    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_id).expect("session should exist");
    assert_eq!(
        session
            .messages
            .last()
            .and_then(|message| message.content.as_deref()),
        Some(input)
    );
}

#[tokio::test]
async fn handle_idle_socket_input_broadcasts_session_list_when_session_set_changes() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (other_tx, mut other_rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let delete_session_id = "broadcast-delete-target".to_string();

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
        sessions.insert(
            delete_session_id.clone(),
            test_session(&delete_session_id, "Delete Target", None),
        );
    }
    {
        let mut clients = state.session_clients.lock().await;
        clients.insert(
            session_id.clone(),
            SessionClientBinding {
                connection_id: 1,
                tx: tx.clone(),
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );
        clients.insert(
            "other-session".to_string(),
            SessionClientBinding {
                connection_id: 2,
                tx: other_tx.clone(),
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );
    }

    let action = handle_idle_socket_input(
        format!("/delete {delete_session_id}"),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));

    let mut current_events = Vec::new();
    for _ in 0..8 {
        current_events.push(recv_json_with_timeout(&mut rx).await);
        let current_types = current_events
            .iter()
            .filter_map(|payload| payload["type"].as_str())
            .collect::<HashSet<_>>();
        if ["system", "session", "session_list"]
            .into_iter()
            .all(|event_type| current_types.contains(event_type))
        {
            break;
        }
    }
    let current_types = current_events
        .iter()
        .map(|payload| payload["type"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(current_types.contains(&"system".to_string()));
    assert!(current_types.contains(&"session".to_string()));
    assert!(current_types.contains(&"session_list".to_string()));

    let other_parsed = recv_json_with_timeout(&mut other_rx).await;
    assert_eq!(other_parsed["type"].as_str(), Some("session_list"));
}

#[tokio::test]
async fn handle_idle_socket_input_broadcasts_model_revision_to_every_session_once() {
    let state = Arc::new(test_app_state());
    let session_id = format!("model-broadcast-current-{}", now_epoch());
    let other_session_id = format!("model-broadcast-other-{}", now_epoch());
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (other_tx, mut other_rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            session_id.clone(),
            test_session(&session_id, "Current", None),
        );
        sessions.insert(
            other_session_id.clone(),
            test_session(&other_session_id, "Other", None),
        );
    }
    {
        let mut clients = state.session_clients.lock().await;
        clients.insert(
            session_id.clone(),
            SessionClientBinding {
                connection_id: 1,
                tx: tx.clone(),
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );
        clients.insert(
            other_session_id.clone(),
            SessionClientBinding {
                connection_id: 2,
                tx: other_tx,
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );
    }
    let (_, revision_before) = state.config_snapshot_with_revision();

    let action = handle_idle_socket_input(
        "/model openai/gpt-4o-mini".into(),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    let current_events = [
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
    ];
    assert_eq!(
        current_events[0]["type"].as_str(),
        Some("session_model_configuration"),
        "the committed global model revision must be delivered before origin-only refresh output"
    );
    assert!(
        current_events
            .iter()
            .any(|payload| payload["type"].as_str() == Some("system"))
    );
    let current_status = current_events
        .iter()
        .find(|payload| payload["type"].as_str() == Some("session_model_configuration"))
        .expect("current Session should receive one model status payload");
    let other_status = recv_json_with_timeout(&mut other_rx).await;
    let revision = current_status["configRevision"]
        .as_u64()
        .expect("current status should carry a revision");
    assert!(revision > revision_before);
    assert_eq!(current_status["id"].as_str(), Some(session_id.as_str()));
    assert_eq!(current_status["modelOverridePresent"], true);
    assert_eq!(current_status["modelOverrideConfigured"], true);
    assert_eq!(
        other_status["type"].as_str(),
        Some("session_model_configuration")
    );
    assert_eq!(other_status["id"].as_str(), Some(other_session_id.as_str()));
    assert_eq!(other_status["modelOverridePresent"], false);
    assert_eq!(other_status["configRevision"], revision);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .is_err(),
        "the current Session must not receive a duplicate direct status payload"
    );

    let _ = tokio::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    )
    .await;
}

#[tokio::test]
async fn model_configuration_send_does_not_let_one_backpressured_session_block_others() {
    let state = Arc::new(test_app_state());
    let blocked_session_id = "blocked-model-status-client".to_string();
    let ready_session_id = "ready-model-status-client".to_string();
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            blocked_session_id.clone(),
            test_session(&blocked_session_id, "Blocked", None),
        );
        sessions.insert(
            ready_session_id.clone(),
            test_session(&ready_session_id, "Ready", None),
        );
    }

    let (blocked_tx, _blocked_rx) = tokio::sync::mpsc::channel::<String>(1);
    blocked_tx
        .send("occupied".to_string())
        .await
        .expect("blocked channel should accept its sentinel");
    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::channel::<String>(2);
    {
        let mut clients = state.session_clients.lock().await;
        clients.insert(
            blocked_session_id,
            SessionClientBinding {
                connection_id: 1,
                tx: blocked_tx,
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );
        clients.insert(
            ready_session_id.clone(),
            SessionClientBinding {
                connection_id: 2,
                tx: ready_tx,
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );
    }
    let model_status_guard = CONFIG_FILE_LOCK.read().await;
    let (config, config_revision) = state.config_snapshot_with_revision();
    let payloads =
        crate::socket_sync::collect_model_configuration_payloads(&state, &config, config_revision)
            .await;
    drop(model_status_guard);

    let send_task = tokio::spawn({
        let state = Arc::clone(&state);
        async move {
            crate::socket_sync::send_model_configuration_payloads(&state, payloads).await;
        }
    });

    let ready_status = recv_json_with_timeout(&mut ready_rx).await;
    assert_eq!(
        ready_status["type"].as_str(),
        Some("session_model_configuration")
    );
    assert_eq!(ready_status["id"].as_str(), Some(ready_session_id.as_str()));
    assert_eq!(ready_status["configRevision"], config_revision);
    assert!(
        !send_task.is_finished(),
        "the sentinel-filled client should keep its own send pending"
    );
    send_task.abort();
    let _ = send_task.await;
}

#[cfg(windows)]
#[test]
fn session_persist_gate_is_shared_by_case_aliases() {
    let upper = crate::session_store::session_persist_gate("CaseAliasGate");
    let lower = crate::session_store::session_persist_gate("casealiasgate");

    assert!(Arc::ptr_eq(&upper, &lower));
}

#[cfg(windows)]
#[tokio::test]
async fn ensure_session_ready_reuses_loaded_session_case_insensitively() {
    let state = Arc::new(test_app_state());
    let session_id = "CaseSensitiveSession".to_string();

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            session_id.clone(),
            test_session(&session_id, "Case Session", None),
        );
    }

    let (resolved, created_fresh) = ensure_session_ready(&state, Some("casesensitivesession"))
        .await
        .expect("case variant should resolve to loaded session");

    assert_eq!(resolved, session_id);
    assert!(!created_fresh);
    let sessions = state.sessions.lock().await;
    assert_eq!(sessions.len(), 1);
    assert!(sessions.contains_key("CaseSensitiveSession"));
    assert!(!sessions.contains_key("casesensitivesession"));
}

#[cfg(not(windows))]
#[tokio::test]
async fn ensure_session_ready_allows_case_distinct_loaded_sessions_on_case_sensitive_platforms() {
    let state = Arc::new(test_app_state());
    let upper_session_id = "CaseDistinctSession".to_string();
    let lower_session_id = "casedistinctsession".to_string();

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            upper_session_id.clone(),
            test_session(&upper_session_id, "Upper Case Session", None),
        );
        sessions.insert(
            lower_session_id.clone(),
            test_session(&lower_session_id, "Lower Case Session", None),
        );
    }

    let (resolved, created_fresh) = ensure_session_ready(&state, Some(&lower_session_id))
        .await
        .expect("exact case session should resolve independently");

    assert_eq!(resolved, lower_session_id);
    assert!(!created_fresh);
}

#[cfg(windows)]
#[tokio::test]
async fn ensure_session_ready_uses_persisted_canonical_id_for_case_alias() {
    let state = Arc::new(test_app_state());
    let session_id = format!("CasePersistedSession{}", now_epoch());
    let workspace = crate::session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let mut session = test_session(&session_id, "Case Persisted", None);
    session.workspace = workspace.clone();
    save_session_to_disk(&session)
        .await
        .expect("session should persist");

    let requested = session_id.to_ascii_lowercase();
    let (resolved, created_fresh) = ensure_session_ready(&state, Some(&requested))
        .await
        .expect("case alias should load persisted canonical session");

    assert_eq!(resolved, session_id);
    assert!(!created_fresh);
    let sessions = state.sessions.lock().await;
    assert_eq!(sessions.len(), 1);
    assert!(sessions.contains_key(&session_id));
    assert!(!sessions.contains_key(&requested));

    let _ = tokio::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    )
    .await;
    let _ = tokio::fs::remove_dir_all(workspace.parent().unwrap()).await;
}

#[tokio::test]
async fn resolve_or_create_socket_session_broadcasts_session_list_for_fresh_session() {
    let state = Arc::new(test_app_state());
    let session_id = format!("fresh-ws-session-{}", now_epoch());
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (other_tx, mut other_rx) = tokio::sync::mpsc::channel::<String>(8);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            MAIN_SESSION_ID.to_string(),
            test_session(MAIN_SESSION_ID, "Main", None),
        );
    }
    {
        let mut clients = state.session_clients.lock().await;
        clients.insert(
            MAIN_SESSION_ID.to_string(),
            SessionClientBinding {
                connection_id: 2,
                tx: other_tx.clone(),
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );
    }

    let connection_cancel = CancellationToken::new();
    let resolved =
        resolve_or_create_socket_session(&state, &tx, Some(&session_id), 1, &connection_cancel)
            .await;

    assert_eq!(resolved, session_id);

    let current_events = [
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
    ];
    let current_types = current_events
        .iter()
        .map(|payload| payload["type"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(current_types.contains(&"session".to_string()));
    assert!(current_types.contains(&"view_state".to_string()));
    assert!(current_types.contains(&"todos_state".to_string()));
    assert!(current_types.contains(&"history".to_string()));
    assert!(current_types.contains(&"feature_status".to_string()));
    assert!(current_types.contains(&"session_group_list".to_string()));
    assert!(current_types.contains(&"session_list".to_string()));

    let other_parsed = recv_json_with_timeout(&mut other_rx).await;
    assert_eq!(other_parsed["type"].as_str(), Some("session_list"));

    let _ = tokio::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    )
    .await;
    let _ = tokio::fs::remove_dir_all(crate::session_workspace_path(&session_id).parent().unwrap())
        .await;
}

#[tokio::test]
async fn resolve_or_create_socket_session_cancels_old_connection_before_replay() {
    let state = Arc::new(test_app_state());
    let session_id = format!("reconnect-cancel-session-{}", now_epoch());
    let old_cancel = CancellationToken::new();
    let new_cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(16);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            session_id.clone(),
            test_session(&session_id, "Reconnect Target", None),
        );
    }
    {
        let mut cancels = state.connection_cancels.lock().await;
        cancels.insert(
            session_id.clone(),
            ConnectionCancelBinding {
                connection_id: 99,
                cancel: old_cancel.clone(),
            },
        );
    }

    let resolved =
        resolve_or_create_socket_session(&state, &tx, Some(&session_id), 1, &new_cancel).await;

    assert_eq!(resolved, session_id);
    assert!(old_cancel.is_cancelled());
    assert!(!new_cancel.is_cancelled());
    {
        let cancels = state.connection_cancels.lock().await;
        let binding = cancels
            .get(&session_id)
            .expect("new cancel binding should be installed before replay");
        assert_eq!(binding.connection_id, 1);
        assert!(!binding.cancel.is_cancelled());
    }

    let current_events = [
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
    ];
    let current_types = current_events
        .iter()
        .map(|payload| payload["type"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(current_types.contains(&"session".to_string()));
    assert!(current_types.contains(&"view_state".to_string()));
    assert!(current_types.contains(&"todos_state".to_string()));
    assert!(current_types.contains(&"feature_status".to_string()));
    assert!(current_types.contains(&"session_group_list".to_string()));
    assert!(current_types.contains(&"history".to_string()));
}

#[tokio::test]
async fn resolve_or_create_socket_session_replays_live_tail_for_running_session() {
    let state = Arc::new(test_app_state());
    let session_id = format!("reconnect-live-session-{}", now_epoch());
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(16);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            session_id.clone(),
            test_session(&session_id, "Reconnect Target", None),
        );
    }
    {
        let mut runs = state.active_runs.lock().await;
        runs.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 99,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
    }
    let old_cancel = CancellationToken::new();
    {
        let mut cancels = state.connection_cancels.lock().await;
        cancels.insert(
            session_id.clone(),
            ConnectionCancelBinding {
                connection_id: 99,
                cancel: old_cancel.clone(),
            },
        );
    }

    dispatch_live_event(
        state.as_ref(),
        &session_id,
        99,
        serde_json::json!({
            "type":"start",
            "round":1,
            "phase":"act",
            "cycle":0,
            "react_visible":false,
        }),
    )
    .await;

    let connection_cancel = CancellationToken::new();
    let resolved =
        resolve_or_create_socket_session(&state, &tx, Some(&session_id), 1, &connection_cancel)
            .await;

    assert_eq!(resolved, session_id);
    assert!(old_cancel.is_cancelled());
    assert!(!connection_cancel.is_cancelled());

    dispatch_live_event(
        state.as_ref(),
        &session_id,
        99,
        serde_json::json!({"type":"delta","content":"tail after reconnect"}),
    )
    .await;

    let current_events = [
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
        recv_json_with_timeout(&mut rx).await,
    ];
    let current_types = current_events
        .iter()
        .map(|payload| payload["type"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(current_types.contains(&"session".to_string()));
    assert!(current_types.contains(&"view_state".to_string()));
    assert!(current_types.contains(&"todos_state".to_string()));
    assert!(current_types.contains(&"feature_status".to_string()));
    assert!(current_types.contains(&"session_group_list".to_string()));
    assert!(current_types.contains(&"history".to_string()));
    assert!(current_types.contains(&"start".to_string()));
    assert!(current_events.iter().any(|payload| {
        payload["type"].as_str() == Some("delta")
            && payload["content"].as_str() == Some("tail after reconnect")
    }));
}

#[tokio::test]
async fn switch_session_broadcasts_session_list_when_session_set_changes() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (other_tx, mut other_rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let new_session_id = format!("broadcast-created-session-{}", now_epoch());

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }
    {
        let mut clients = state.session_clients.lock().await;
        clients.insert(
            session_id.clone(),
            SessionClientBinding {
                connection_id: 1,
                tx: tx.clone(),
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );
        clients.insert(
            "other-session".to_string(),
            SessionClientBinding {
                connection_id: 2,
                tx: other_tx.clone(),
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );
    }

    let action = handle_idle_socket_input(
        format!("/switch {new_session_id}"),
        &mut current_session_id,
        &current_session_ref,
        1,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    let IdleSocketInputAction::SwitchSession { session_id, result } = action else {
        panic!("switch command should request session switch");
    };

    assert_eq!(session_id, new_session_id);

    let created_fresh = switch_socket_session(
        &state,
        &tx,
        &current_session_ref,
        &mut current_session_id,
        &CancellationToken::new(),
        1,
        session_id,
    )
    .await
    .expect("session switch should succeed");

    if result.session_list_changed || created_fresh {
        broadcast_session_list_payload(&state).await;
    }
    ws_send(
        &tx,
        &serde_json::json!({
            "type": result.response_type,
            "content": result.response,
            "dismissible": result.dismissible,
        }),
    )
    .await;

    {
        let mut current_payloads = Vec::new();
        while let Ok(Some(payload)) =
            tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await
        {
            current_payloads.push(payload);
        }
        let current_types = current_payloads
            .iter()
            .map(|payload| {
                serde_json::from_str::<serde_json::Value>(payload).expect("payload json")["type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect::<Vec<_>>();
        assert!(current_types.contains(&"session".to_string()));
    }

    let other_parsed = recv_json_with_timeout(&mut other_rx).await;
    assert_eq!(other_parsed["type"].as_str(), Some("session_list"));

    let _ = tokio::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{new_session_id}.json")),
    )
    .await;
    let _ = tokio::fs::remove_dir_all(
        crate::session_workspace_path(&new_session_id)
            .parent()
            .unwrap(),
    )
    .await;
}

#[tokio::test]
async fn handle_idle_socket_input_stop_cancels_reconnected_run() {
    let state = Arc::new(test_app_state());
    let run_cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }
    {
        let mut runs = state.active_runs.lock().await;
        runs.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: run_cancel.clone(),
                stop_requested: stop_requested.clone(),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
    }

    let action = handle_idle_socket_input(
        "/stop".into(),
        &mut current_session_id,
        &current_session_ref,
        2,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));
    assert!(run_cancel.is_cancelled());
    assert!(stop_requested.load(std::sync::atomic::Ordering::Relaxed));

    let payload = rx.recv().await.expect("stop ack should be sent");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
    assert_eq!(parsed["type"].as_str(), Some("system"));
    assert_eq!(parsed["content"].as_str(), Some("Stop requested."));
}

#[tokio::test]
async fn execute_tool_with_live_output_drains_events_before_return() {
    let workspace =
        std::env::temp_dir().join(format!("lingclaw-runtime-live-output-{}", now_epoch()));
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let args = if cfg!(windows) {
        serde_json::json!({
            "program": "cmd",
            "args": ["/C", "echo runtime-live"],
        })
    } else {
        serde_json::json!({
            "program": "sh",
            "args": ["-c", "printf 'runtime-live'"],
        })
    };
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let outcome = execute_tool_with_live_output(
        &live_tx,
        "exec_call_1",
        tools::TOOL_NAME_EXEC,
        &serde_json::to_string(&args).expect("args should serialize"),
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        &workspace,
        false,
        None,
        None,
    )
    .await;

    assert!(!outcome.is_error, "exec wrapper should succeed");

    let mut saw_tool_output = false;
    while let Ok(event) = live_rx.try_recv() {
        if event["type"].as_str() == Some("tool_output") {
            saw_tool_output = true;
            assert_eq!(event["id"].as_str(), Some("exec_call_1"));
            assert_eq!(event["name"].as_str(), Some(tools::TOOL_NAME_EXEC));
            assert!(
                event["chunk"]
                    .as_str()
                    .is_some_and(|chunk| chunk.contains("runtime-live"))
            );
        }
    }
    assert!(
        saw_tool_output,
        "live output should be forwarded before return"
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_with_live_output_preserves_queued_events() {
    let workspace =
        std::env::temp_dir().join(format!("lingclaw-runtime-live-output-full-{}", now_epoch()));
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let args = if cfg!(windows) {
        serde_json::json!({
            "program": "cmd",
            "args": ["/C", "echo runtime-live-full"],
        })
    } else {
        serde_json::json!({
            "program": "sh",
            "args": ["-c", "printf 'runtime-live-full'"],
        })
    };
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    live_tx
        .send(serde_json::json!({"type":"sentinel"}))
        .await
        .expect("sentinel should queue");

    let outcome = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        execute_tool_with_live_output(
            &live_tx,
            "exec_call_full",
            tools::TOOL_NAME_EXEC,
            &serde_json::to_string(&args).expect("args should serialize"),
            &test_config(),
            &reqwest::Client::new(),
            &workspace,
            &workspace,
            false,
            None,
            None,
        ),
    )
    .await
    .expect("exec wrapper should not block when live events are already queued");

    assert!(!outcome.is_error, "exec wrapper should still succeed");
    assert_eq!(
        live_rx.try_recv().expect("sentinel should remain queued")["type"],
        "sentinel"
    );
    let tool_output = live_rx
        .try_recv()
        .expect("tool output should remain queued after the sentinel");
    assert_eq!(tool_output["type"].as_str(), Some("tool_output"));
    assert_eq!(tool_output["id"].as_str(), Some("exec_call_full"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_with_live_output_returns_when_live_queue_is_full() {
    let workspace = std::env::temp_dir().join(format!(
        "lingclaw-runtime-live-output-blocked-{}",
        now_epoch()
    ));
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let args = if cfg!(windows) {
        serde_json::json!({
            "program": "cmd",
            "args": ["/C", "echo runtime-live-blocked"],
        })
    } else {
        serde_json::json!({
            "program": "sh",
            "args": ["-c", "printf 'runtime-live-blocked'"],
        })
    };
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    for _ in 0..LIVE_EVENT_CHANNEL_CAPACITY {
        live_tx
            .send(serde_json::json!({"type":"sentinel"}))
            .await
            .expect("sentinel should queue");
    }

    let outcome = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        execute_tool_with_live_output(
            &live_tx,
            "exec_call_blocked",
            tools::TOOL_NAME_EXEC,
            &serde_json::to_string(&args).expect("args should serialize"),
            &test_config(),
            &reqwest::Client::new(),
            &workspace,
            &workspace,
            false,
            None,
            None,
        ),
    )
    .await
    .expect("exec wrapper should not wait on a full live queue");

    assert!(!outcome.is_error, "exec wrapper should still succeed");
    assert_eq!(
        live_rx.try_recv().expect("sentinel should remain queued")["type"],
        "sentinel"
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_with_live_output_drops_extra_events_when_local_live_queue_is_full() {
    let workspace = std::env::temp_dir().join(format!(
        "lingclaw-runtime-live-output-local-cap-{}",
        now_epoch()
    ));
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let payload = "X".repeat(12_000);
    let script = format!("import sys\nsys.stdout.write({payload:?})");
    let args = if cfg!(windows) {
        serde_json::json!({
            "program": "python",
            "args": ["-c", script],
        })
    } else {
        serde_json::json!({
            "program": "python3",
            "args": ["-c", script],
        })
    };
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    let outcome = execute_tool_with_live_output(
        &live_tx,
        "exec_call_local_cap",
        tools::TOOL_NAME_EXEC,
        &serde_json::to_string(&args).expect("args should serialize"),
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        &workspace,
        false,
        None,
        None,
    )
    .await;

    assert!(outcome.output.contains("exit code:"), "{}", outcome.output);
    let event_count = std::iter::from_fn(|| live_rx.try_recv().ok()).count();
    assert!(event_count <= tools::TOOL_LIVE_EVENT_CHANNEL_CAPACITY);

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn handle_idle_socket_input_queues_new_prompt_while_reconnected_run_active() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let deferred_interventions = Arc::new(Mutex::new(DeferredInterventionState::open()));

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }
    let before_len = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .expect("session should exist")
            .messages
            .len()
    };
    {
        let mut runs = state.active_runs.lock().await;
        runs.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: deferred_interventions.clone(),
            },
        );
    }

    let action = handle_idle_socket_input(
        "hello after refresh".into(),
        &mut current_session_id,
        &current_session_ref,
        2,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));

    let payload = rx.recv().await.expect("busy message should be sent");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
    assert_eq!(parsed["type"].as_str(), Some("progress"));
    assert!(
        parsed["content"]
            .as_str()
            .expect("content should be string")
            .contains("Intervention received")
    );

    let after_len = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .expect("session should exist")
            .messages
            .len()
    };
    assert_eq!(after_len, before_len);

    let queued = deferred_interventions.lock().await;
    assert!(queued.accepting);
    assert_eq!(queued.queue, vec!["hello after refresh".to_string()]);
}

#[tokio::test]
async fn handle_idle_socket_input_allows_think_command_while_reconnected_run_active() {
    let state = Arc::new(test_app_state_with_config(test_reasoning_config()));
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let deferred_interventions = Arc::new(Mutex::new(DeferredInterventionState::open()));

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }
    {
        let mut runs = state.active_runs.lock().await;
        runs.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: deferred_interventions.clone(),
            },
        );
    }

    let action = handle_idle_socket_input(
        "/think high".into(),
        &mut current_session_id,
        &current_session_ref,
        2,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));

    let payload = rx.recv().await.expect("think response should be sent");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
    assert_eq!(parsed["type"].as_str(), Some("system"));
    assert!(
        parsed["content"]
            .as_str()
            .is_some_and(|value| value.contains("Think mode set to: high"))
    );
    assert!(
        parsed["content"]
            .as_str()
            .is_some_and(|value| value.contains("next reasoning cycle"))
    );

    let updated_think = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .expect("session should exist")
            .think_level
            .clone()
    };
    assert_eq!(updated_think, "high");

    let queued = deferred_interventions.lock().await;
    assert!(queued.queue.is_empty());
}

#[tokio::test]
async fn handle_idle_socket_input_rejects_intervention_when_reconnected_run_is_finishing() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let deferred_interventions = Arc::new(Mutex::new(DeferredInterventionState {
        queue: Vec::new(),
        accepting: false,
    }));

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }
    {
        let mut runs = state.active_runs.lock().await;
        runs.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: deferred_interventions.clone(),
            },
        );
    }

    let action = handle_idle_socket_input(
        "late intervention".into(),
        &mut current_session_id,
        &current_session_ref,
        2,
        &state,
        &tx,
        &live_tx,
        &cancel,
        &Arc::new(AtomicBool::new(false)),
    )
    .await;

    assert!(matches!(action, IdleSocketInputAction::Continue));

    let payload = rx.recv().await.expect("finishing message should be sent");
    let parsed: serde_json::Value = serde_json::from_str(&payload).expect("payload json");
    assert_eq!(parsed["type"].as_str(), Some("system"));
    assert!(
        parsed["content"]
            .as_str()
            .expect("content should be string")
            .contains("already finishing")
    );

    let queued = deferred_interventions.lock().await;
    assert!(queued.queue.is_empty());
}

#[tokio::test]
async fn run_agent_session_emits_user_stop_done_for_shared_stop_request() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(true));
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let (_inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<String>(4);

    {
        let mut sessions = state.sessions.lock().await;
        let mut session = test_session(&session_id, "Main", None);
        session.messages.push(ChatMessage {
            role: "user".into(),
            content: Some("stop this active run".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(1),
        });
        sessions.insert(session_id.clone(), session);
    }

    let outcome = run_agent_session(
        &state,
        &session_id,
        1,
        &cancel,
        &live_tx,
        &mut inbound_rx,
        &stop_requested,
        AgentRunMode::Execute,
        None,
        None,
    )
    .await;

    assert!(!outcome.rerun_agent);
    assert!(!outcome.shutting_down);

    let done_event = live_rx.recv().await.expect("done event should be emitted");
    assert_eq!(done_event["type"].as_str(), Some("done"));
    assert_eq!(done_event["phase"].as_str(), Some("stopped"));
    assert_eq!(done_event["reason"].as_str(), Some("user_stop"));
}

#[tokio::test]
async fn stopped_run_with_ambiguous_anchor_emits_one_live_incomplete_terminal() {
    let state = Arc::new(test_app_state());
    let session_id = "stopped-ambiguous-anchor".to_string();
    let cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(true));
    let (live_tx, mut live_rx) = mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let (_inbound_tx, mut inbound_rx) = mpsc::channel::<String>(4);
    let duplicate = ChatMessage {
        role: "user".into(),
        content: Some("stop this duplicate input".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(7),
    };
    let mut session = test_session(&session_id, "Stopped ambiguous", None);
    session.messages.push(duplicate.clone());
    session.messages.push(duplicate);
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let outcome = run_agent_session(
        &state,
        &session_id,
        1,
        &cancel,
        &live_tx,
        &mut inbound_rx,
        &stop_requested,
        AgentRunMode::Execute,
        None,
        None,
    )
    .await;

    assert!(outcome.run_stopped);
    assert!(outcome.run_failed);
    assert!(!state.active_runs.lock().await.contains_key(&session_id));
    let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
    let terminal = events
        .iter()
        .filter(|event| event["run_terminal"] == true)
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0]["code"], "terminal_identity_unavailable");
    assert_eq!(terminal[0]["phase"], "incomplete");
    assert!(!events.iter().any(|event| event["type"] == "done"));
    assert!(
        take_terminal_persistence_record(&session_id)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn run_agent_session_rechecks_model_configuration_at_the_run_boundary() {
    let mut config = test_config();
    config.explicit_primary_model_configured = false;
    let state = Arc::new(test_app_state());
    state.replace_config(config);
    let session_id = MAIN_SESSION_ID.to_string();
    let cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let (_inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<String>(4);
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), test_session(&session_id, "Main", None));
    }

    let outcome = run_agent_session(
        &state,
        &session_id,
        1,
        &cancel,
        &live_tx,
        &mut inbound_rx,
        &stop_requested,
        AgentRunMode::Execute,
        None,
        None,
    )
    .await;

    assert!(outcome.run_failed);
    assert!(!outcome.rerun_agent);
    let event = live_rx
        .recv()
        .await
        .expect("model gate error should be emitted");
    assert_eq!(event["type"], "error");
    assert!(!state.active_runs.lock().await.contains_key(&session_id));
}

#[tokio::test]
async fn run_agent_session_marks_approved_plan_failed_when_workspace_is_unavailable() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "missing-workspace-plan-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let missing_workspace = temp_workspace("missing-approved-plan-workspace");
    let mut session = test_session(&session_id, "Missing Plan Workspace", None);
    session.workspace_kind = crate::SessionWorkspaceKind::Directory;
    session.working_directory = missing_workspace;
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_missing_workspace".into(),
        revision: 2,
        status: crate::plan::PlanStatus::Executing,
        approved_at: Some(20),
        execution_attempt: 1,
        created_at: 10,
        updated_at: 20,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let (_inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<String>(4);

    let outcome = run_agent_session(
        &state,
        &session_id,
        1,
        &cancel,
        &live_tx,
        &mut inbound_rx,
        &stop_requested,
        AgentRunMode::Execute,
        None,
        None,
    )
    .await;

    assert!(outcome.run_failed);
    assert!(!state.active_runs.lock().await.contains_key(&session_id));
    let sessions = state.sessions.lock().await;
    let plan = sessions[&session_id]
        .pending_plan
        .as_ref()
        .expect("approved plan should remain visible");
    assert_eq!(plan.status, crate::plan::PlanStatus::Failed);
    assert!(plan.finished_at.is_some());
    drop(sessions);

    let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .any(|event| { event["type"] == "plan_state" && event["plan"]["status"] == "failed" })
    );
    assert!(
        events
            .iter()
            .any(|event| { event["type"] == "error" && event["code"] == "workspace_unavailable" })
    );

    crate::session_store::delete_session_from_storage(&session_id)
        .await
        .expect("test session should be removed");
}

#[tokio::test]
async fn run_agent_session_prioritizes_stop_request_over_cancel() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let stop_requested = Arc::new(AtomicBool::new(true));
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let (_inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<String>(4);

    {
        let mut sessions = state.sessions.lock().await;
        let mut session = test_session(&session_id, "Main", None);
        session.messages.push(ChatMessage {
            role: "user".into(),
            content: Some("stop even if the connection is also cancelled".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(2),
        });
        sessions.insert(session_id.clone(), session);
    }

    let outcome = run_agent_session(
        &state,
        &session_id,
        1,
        &cancel,
        &live_tx,
        &mut inbound_rx,
        &stop_requested,
        AgentRunMode::Execute,
        None,
        None,
    )
    .await;

    assert!(!outcome.rerun_agent);
    assert!(!outcome.shutting_down);
    assert!(outcome.run_stopped);

    let done_event = live_rx.recv().await.expect("done event should be emitted");
    assert_eq!(done_event["type"].as_str(), Some("done"));
    assert_eq!(done_event["phase"].as_str(), Some("stopped"));
    assert_eq!(done_event["reason"].as_str(), Some("user_stop"));
    assert!(
        live_rx.try_recv().is_err(),
        "stop should not emit a shutdown message"
    );
}

#[tokio::test]
async fn run_agent_session_stop_preserves_interventions_after_trimming_incomplete_tools() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let (live_tx, mut live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel::<String>(4);

    let mut session = test_session(&session_id, "Main", None);
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("run both tool calls".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(3),
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![
            ToolCall {
                id: "call-1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: r#"{"command":"echo one"}"#.into(),
                },
            },
            ToolCall {
                id: "call-2".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: r#"{"command":"echo two"}"#.into(),
                },
            },
        ]),
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "tool".into(),
        content: Some("one".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: Some("call-1".into()),
        timestamp: None,
    });

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }
    inbound_tx
        .send("follow-up detail".into())
        .await
        .expect("intervention should queue");
    inbound_tx
        .send("/stop".into())
        .await
        .expect("stop should queue");

    let outcome = run_agent_session(
        &state,
        &session_id,
        1,
        &cancel,
        &live_tx,
        &mut inbound_rx,
        &stop_requested,
        AgentRunMode::Execute,
        None,
        None,
    )
    .await;

    assert!(!outcome.rerun_agent);
    assert!(!outcome.shutting_down);

    let progress_event = live_rx
        .recv()
        .await
        .expect("progress event should be emitted");
    assert_eq!(progress_event["type"].as_str(), Some("progress"));
    let done_event = live_rx.recv().await.expect("done event should be emitted");
    assert_eq!(done_event["type"].as_str(), Some("done"));
    assert_eq!(done_event["phase"].as_str(), Some("stopped"));

    let persisted = state
        .sessions
        .lock()
        .await
        .get(&session_id)
        .cloned()
        .expect("session should exist");
    assert_eq!(persisted.messages.len(), 3);
    assert_eq!(persisted.messages[0].role, "system");
    assert_eq!(persisted.messages[1].role, "user");
    assert_eq!(
        persisted.messages[1].content.as_deref(),
        Some("run both tool calls")
    );
    assert_eq!(persisted.messages[2].role, "user");
    assert_eq!(
        persisted.messages[2].content.as_deref(),
        Some("follow-up detail")
    );
}

#[tokio::test]
async fn active_run_stop_relay_is_bound_to_the_exact_stop_generation() {
    let old_stop = Arc::new(AtomicBool::new(false));
    let replacement_stop = Arc::new(AtomicBool::new(false));
    let run_cancel = CancellationToken::new();
    let relay_shutdown = CancellationToken::new();
    let relay = tokio::spawn(relay_active_run_stop_to_cancellation(
        Arc::clone(&old_stop),
        run_cancel.clone(),
        relay_shutdown.clone(),
    ));

    replacement_stop.store(true, Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(
        !run_cancel.is_cancelled(),
        "a replacement generation must not cancel the active run"
    );

    old_stop.store(true, Ordering::Relaxed);
    tokio::time::timeout(Duration::from_secs(1), run_cancel.cancelled())
        .await
        .expect("the exact stop generation should cancel promptly");
    relay_shutdown.cancel();
    relay.await.expect("stop relay should finish cleanly");
}

#[tokio::test]
async fn apply_run_cancel_outcome_treats_shared_stop_as_user_stop() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(true));
    let deferred_interventions = Arc::new(Mutex::new(DeferredInterventionState::open()));
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    {
        let mut runs = state.active_runs.lock().await;
        runs.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: run_cancel.clone(),
                stop_requested: stop_requested.clone(),
                deferred_interventions,
            },
        );
    }

    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        results_origin_query: None,
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: None,
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        unresolved_tool_targets: HashSet::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    };

    apply_run_cancel_outcome(&ctx, &mut phase_state).await;

    assert!(phase_state.run_stopped);
    assert!(!phase_state.run_detached);
    assert!(!stop_requested.load(std::sync::atomic::Ordering::Relaxed));
}

#[tokio::test]
async fn apply_run_cancel_outcome_treats_storage_cancellation_as_detach() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let deferred_interventions = Arc::new(Mutex::new(DeferredInterventionState::open()));
    let (live_tx, _live_rx) =
        tokio::sync::mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);

    {
        let mut runs = state.active_runs.lock().await;
        runs.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: run_cancel.clone(),
                stop_requested: stop_requested.clone(),
                deferred_interventions,
            },
        );
    }

    run_cancel.cancel();
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();

    apply_run_cancel_outcome(&ctx, &mut phase_state).await;

    assert!(!phase_state.run_stopped);
    assert!(phase_state.run_detached);
    assert!(!phase_state.shutting_down);
    assert!(!stop_requested.load(std::sync::atomic::Ordering::Relaxed));
}

fn test_app_state_with_config(config: Config) -> AppState {
    AppState {
        config: std::sync::Mutex::new(Arc::new(config)),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        group_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        session_control_locks: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: HookRegistry::new(),
        auxiliary_tasks: crate::auxiliary_tasks::AuxiliaryTaskRegistry::new(true, true),
        memory_queue: std::sync::Mutex::new(None),
    }
}

fn test_session(id: &str, name: &str, model_override: Option<&str>) -> Session {
    let workspace = std::env::temp_dir();
    Session {
        id: id.to_string(),
        name: name.to_string(),
        messages: vec![ChatMessage {
            role: "system".into(),
            content: Some("system".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        }],
        created_at: 0,
        updated_at: 0,
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
        model_override: model_override.map(|value| value.to_string()),
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        pending_plan: None,
        version: 0,
        working_directory: workspace.clone(),
        workspace_kind: crate::SessionWorkspaceKind::Managed,
        workspace,
    }
}

fn temp_workspace(label: &str) -> PathBuf {
    let unique = format!(
        "lingclaw-runtime-loop-{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

fn completion_verifier_test_plan(id: &str) -> crate::PendingPlan {
    crate::PendingPlan {
        id: id.into(),
        revision: 2,
        status: crate::plan::PlanStatus::Executing,
        artifact: crate::plan::PlanArtifact {
            title: "Verify completion safely".into(),
            goal: "Finish only after bounded verification".into(),
            steps: vec![crate::plan::PlanStep {
                id: "verify".into(),
                title: "Verify the approved result".into(),
                ..Default::default()
            }],
            verification: vec!["The approved workspace remains available.".into()],
            acceptance_criteria: vec!["The approved workspace remains available.".into()],
            completion_checks: vec![crate::plan::PlanCompletionCheck {
                id: "verify-workspace".into(),
                step_id: "verify".into(),
                covers: vec![
                    crate::plan::PlanContractClauseRef {
                        section: crate::plan::PlanContractSection::Verification,
                        index: 0,
                    },
                    crate::plan::PlanContractClauseRef {
                        section: crate::plan::PlanContractSection::AcceptanceCriteria,
                        index: 0,
                    },
                ],
                kind: crate::plan::PlanCompletionCheckKind::WorkspacePath,
                path: Some(".".into()),
                expected_path_type: Some(crate::plan::PlanExpectedPathType::Directory),
                exact_content: None,
                size_bytes: None,
                sha256: None,
                evidence_kind: None,
                required_step_ids: Vec::new(),
                tool_name: None,
                arguments: None,
            }],
            ..Default::default()
        },
        progress: vec![crate::plan::PlanProgressStep {
            id: "verify".into(),
            title: "Verify the approved result".into(),
            status: crate::plan::PlanStepStatus::Completed,
            note: "Verified".into(),
            deviation_reason: None,
        }],
        approved_at: Some(20),
        execution_attempt: 1,
        created_at: 10,
        updated_at: 20,
        ..Default::default()
    }
}

async fn wait_for_completion_verifier_gate(gate: &CompletionVerifierTestGate) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !gate.started.load(Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completion verifier should start");
}

async fn wait_for_completion_verifier_stop(gate: &CompletionVerifierTestGate) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !gate.stopped.load(Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completion verifier should stop cooperatively");
}

async fn wait_for_finish_phase_gate(gate: &FinishPhaseTestGate) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !gate.started.load(Ordering::Relaxed) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("Finish phase test gate should start");
}

struct FinishPhaseGateRelease(Arc<FinishPhaseTestGate>);

impl Drop for FinishPhaseGateRelease {
    fn drop(&mut self) {
        self.0.release.store(true, Ordering::Relaxed);
    }
}

struct RuntimeOutcomeGateRelease(Arc<crate::storage::RunOutcomeCommitTestGate>);

impl Drop for RuntimeOutcomeGateRelease {
    fn drop(&mut self) {
        self.0.allow_commit();
        self.0.allow_response();
    }
}

async fn wait_for_runtime_outcome_gate(condition: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("real SQLite terminal barrier should be reached");
}

fn terminal_merge_test_outcome(
    session: &Session,
    run_id: &str,
    status: &str,
    phase: &str,
) -> crate::TopLevelRunOutcome {
    crate::TopLevelRunOutcome {
        diagnostic: None,
        session_id: session.id.clone(),
        run_id: run_id.to_string(),
        run_connection_id: "terminal-merge-connection".to_string(),
        status: status.to_string(),
        phase: phase.to_string(),
        reason: Some(phase.to_string()),
        duration_ms: 45,
        start_message_index: 1,
        end_message_index: session.messages.len().saturating_sub(1),
        plan_id: session.pending_plan.as_ref().map(|plan| plan.id.clone()),
        plan_revision: session.pending_plan.as_ref().map(|plan| plan.revision),
        started_at: 100,
        finished_at: 101,
    }
}

struct RuntimeLoopTestArtifactsGuard {
    session_id: String,
    workspace: PathBuf,
}

struct ManagedSessionArtifactsGuard {
    session_id: String,
}

struct ReflectionRuntimeReset;

impl Drop for ReflectionRuntimeReset {
    fn drop(&mut self) {
        refresh_reflection_runtime(false);
    }
}

impl ManagedSessionArtifactsGuard {
    fn new(session_id: &str) -> Self {
        let workspace = crate::session_workspace_path(session_id);
        if let Some(session_root) = workspace.parent() {
            let _ = std::fs::remove_dir_all(session_root);
        }
        let session_path = crate::session_store::sessions_dir().join(format!("{session_id}.json"));
        for path in [
            session_path.clone(),
            session_path.with_extension("json.tmp"),
            session_path.with_extension("json.lingclaw-save-backup"),
        ] {
            let _ = std::fs::remove_file(path);
        }
        Self {
            session_id: session_id.to_string(),
        }
    }
}

impl Drop for ManagedSessionArtifactsGuard {
    fn drop(&mut self) {
        let session_path =
            crate::session_store::sessions_dir().join(format!("{}.json", self.session_id));
        for path in [
            session_path.clone(),
            session_path.with_extension("json.tmp"),
            session_path.with_extension("json.lingclaw-save-backup"),
        ] {
            let _ = std::fs::remove_file(path);
        }
        if let Some(session_root) = crate::session_workspace_path(&self.session_id).parent() {
            let _ = std::fs::remove_dir_all(session_root);
        }
    }
}

impl RuntimeLoopTestArtifactsGuard {
    fn new(session_id: &str, workspace: &Path) -> Self {
        assert!(
            workspace.starts_with(std::env::temp_dir()),
            "runtime loop test workspaces must stay under the process temp directory"
        );
        Self {
            session_id: session_id.to_string(),
            workspace: workspace.to_path_buf(),
        }
    }
}

impl Drop for RuntimeLoopTestArtifactsGuard {
    fn drop(&mut self) {
        let session_path =
            crate::session_store::sessions_dir().join(format!("{}.json", self.session_id));
        for path in [
            session_path.clone(),
            session_path.with_extension("json.tmp"),
            session_path.with_extension("json.lingclaw-save-backup"),
        ] {
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_dir_all(&self.workspace);
        if let Some(managed_session_dir) = crate::session_workspace_path(&self.session_id).parent()
        {
            let _ = std::fs::remove_dir_all(managed_session_dir);
        }
    }
}

fn phase_state_for_analyze_test() -> AgentPhaseState {
    AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        results_origin_query: None,
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: None,
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        unresolved_tool_targets: HashSet::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(true),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    }
}

fn install_openai_model(state: &Arc<AppState>, model_id: &str, reasoning: bool) {
    let mut guard = state.config.lock().expect("config lock");
    let mut config = (**guard).clone();
    config.model = format!("openai/{model_id}");
    config.providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "openai-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: model_id.to_string(),
                name: None,
                reasoning: Some(reasoning),
                effort: None,
                input: None,
                cost: None,
                context_window: Some(8192),
                max_tokens: Some(2048),
                compat: None,
            }],
        },
    );
    *guard = Arc::new(config);
}

#[test]
fn append_dynamic_prompt_section_truncates_to_budget() {
    let mut content = "system".to_string();
    let mut remaining_budget = 72;
    let appended = append_dynamic_prompt_section(
        &mut content,
        &mut remaining_budget,
        "## Big Section\nThis section should be truncated because it is far too long for the remaining budget.",
    );

    assert!(appended);
    assert_eq!(remaining_budget, 0);
    assert!(content.contains("## Big Section"));
    assert!(content.contains("dynamic context truncated"));
}

#[tokio::test]
async fn report_working_state_digest_issue_emits_warning_event() {
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);

    report_working_state_digest_issue(&live_tx, "fast-model", WorkingStateDigestIssue::Timeout)
        .await;

    let event = live_rx
        .recv()
        .await
        .expect("digest warning event should be emitted");
    assert_eq!(event["type"].as_str(), Some("system"));
    assert_eq!(event["level"].as_str(), Some("warning"));
    assert_eq!(event["source"].as_str(), Some("working_state_digest"));
    assert_eq!(event["reason"].as_str(), Some("timeout"));
    assert_eq!(event["model"].as_str(), Some("fast-model"));
    assert!(
        event["content"]
            .as_str()
            .is_some_and(|content| content.contains("rule-based state tracking"))
    );
}

#[tokio::test]
async fn run_analyze_phase_emits_start_then_auto_trace_for_auto_rounds() {
    let state = Arc::new(test_app_state());
    install_openai_model(&state, "gpt-4o-reasoner", true);

    let session_id = "auto-trace-analyze-order".to_string();
    let workspace = temp_workspace("auto-trace-analyze-order");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", Some("openai/gpt-4o-reasoner"));
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some(format!(
            "investigate the timeout loop and explain the blockers {}",
            "A".repeat(12_000)
        )),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.working_state.seed_from_query(Some(
        "investigate the timeout loop and explain the blockers",
    ));

    let control = run_analyze_phase(&ctx, &mut phase_state).await;
    assert!(matches!(control, AgentPhaseControl::Break));

    let start = live_rx.recv().await.expect("start event should be emitted");
    let task_plan = live_rx.recv().await.expect("task plan should follow");
    let auto_trace = live_rx.recv().await.expect("auto trace should follow");
    let error = live_rx
        .recv()
        .await
        .expect("budget error should stop the round");

    assert_eq!(start["type"].as_str(), Some("start"));
    assert_eq!(task_plan["type"].as_str(), Some("task_plan"));
    assert_eq!(task_plan["round"].as_u64(), Some(1));
    assert_eq!(task_plan["cycle"].as_u64(), Some(0));
    assert_eq!(auto_trace["type"].as_str(), Some("auto_trace"));
    assert_eq!(auto_trace["round"].as_u64(), Some(1));
    assert_eq!(auto_trace["cycle"].as_u64(), Some(0));
    assert_eq!(auto_trace["model"].as_str(), Some("openai/gpt-4o-reasoner"));
    assert_eq!(auto_trace["provider"].as_str(), Some("openai"));
    assert!(auto_trace["selected_think"].as_str().is_some());
    assert_eq!(error["type"].as_str(), Some("error"));
    assert_eq!(error["run_terminal"].as_bool(), Some(true));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn run_analyze_phase_emits_post_hook_think_in_auto_trace() {
    let mut hooks = HookRegistry::new();
    hooks.register(Box::new(ThinkOverrideHook {
        new_level: "off".to_string(),
    }));

    let state = Arc::new(test_app_state_with_hooks(hooks));
    install_openai_model(&state, "gpt-4o-reasoner", true);

    let session_id = "auto-trace-think-override".to_string();
    let workspace = temp_workspace("auto-trace-think-override");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", Some("openai/gpt-4o-reasoner"));
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some(format!(
            "investigate the timeout loop and explain the blockers {}",
            "D".repeat(12_000)
        )),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.working_state.seed_from_query(Some(
        "investigate the timeout loop and explain the blockers",
    ));

    let control = run_analyze_phase(&ctx, &mut phase_state).await;
    assert!(matches!(control, AgentPhaseControl::Break));

    let start = live_rx.recv().await.expect("start event should be emitted");
    let task_plan = live_rx.recv().await.expect("task plan should follow");
    let auto_trace = live_rx.recv().await.expect("auto trace should follow");
    let error = live_rx
        .recv()
        .await
        .expect("budget error should stop the round");

    assert_eq!(start["type"].as_str(), Some("start"));
    assert_eq!(start["think_level"].as_str(), Some("off"));
    assert_eq!(task_plan["type"].as_str(), Some("task_plan"));
    assert_eq!(auto_trace["type"].as_str(), Some("auto_trace"));
    assert_eq!(auto_trace["selected_think"].as_str(), Some("off"));
    assert!(auto_trace["clamps"].as_array().is_some_and(|clamps| {
        clamps
            .iter()
            .any(|value| value.as_str() == Some("hook_think_override"))
    }));
    assert_eq!(error["type"].as_str(), Some("error"));
    assert_eq!(error["run_terminal"].as_bool(), Some(true));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn run_analyze_phase_skips_auto_trace_for_manual_think_levels() {
    let state = Arc::new(test_app_state());
    install_openai_model(&state, "gpt-4o-reasoner", true);

    let session_id = "manual-think-no-auto-trace".to_string();
    let workspace = temp_workspace("manual-think-no-auto-trace");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", Some("openai/gpt-4o-reasoner"));
    session.workspace = workspace.clone();
    session.think_level = "high".to_string();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some(format!("change the runtime loop {}", "B".repeat(12_000))),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state
        .working_state
        .seed_from_query(Some("change the runtime loop"));

    let control = run_analyze_phase(&ctx, &mut phase_state).await;
    assert!(matches!(control, AgentPhaseControl::Break));

    let first = live_rx.recv().await.expect("start event should be emitted");
    let second = live_rx
        .recv()
        .await
        .expect("round should end with budget error");

    assert_eq!(first["type"].as_str(), Some("start"));
    assert_ne!(second["type"].as_str(), Some("auto_trace"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn run_analyze_phase_skips_auto_trace_for_unsupported_models() {
    let state = Arc::new(test_app_state());
    install_openai_model(&state, "gpt-4o-mini", false);

    let session_id = "unsupported-auto-no-trace".to_string();
    let workspace = temp_workspace("unsupported-auto-no-trace");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", Some("openai/gpt-4o-mini"));
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some(format!("summarize the runtime loop {}", "C".repeat(12_000))),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state
        .working_state
        .seed_from_query(Some("summarize the runtime loop"));

    let control = run_analyze_phase(&ctx, &mut phase_state).await;
    assert!(matches!(control, AgentPhaseControl::Break));

    let first = live_rx.recv().await.expect("start event should be emitted");
    let second = live_rx
        .recv()
        .await
        .expect("round should end with budget error");

    assert_eq!(first["type"].as_str(), Some("start"));
    assert_ne!(second["type"].as_str(), Some("auto_trace"));
    assert_eq!(first["think_level"].as_str(), Some("off"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn prepare_analyze_snapshot_preserves_messages_for_before_analyze_compression() {
    let state = Arc::new(test_app_state());
    let session_id = "snapshot-preserves-history".to_string();
    let workspace = temp_workspace("snapshot-preserves-history");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    for idx in 0..18 {
        session.messages.push(ChatMessage {
            role: "user".into(),
            content: Some(format!("old question {idx} {}", "A".repeat(400))),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        });
        session.messages.push(ChatMessage {
            role: "assistant".into(),
            content: Some(format!("old answer {idx} {}", "B".repeat(400))),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        });
    }
    let original_len = session.messages.len();
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state
        .working_state
        .seed_from_query(Some("compress the old conversation"));

    let snapshot = prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared");

    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_id).expect("session should exist");
    assert_eq!(session.messages.len(), original_len);
    assert_eq!(snapshot.pruned_count, 0);
    assert!(crate::hooks::find_auto_compress_cutoff(&session.messages, 8).is_some());

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn prepare_analyze_snapshot_includes_task_plan_in_execute_mode() {
    let state = Arc::new(test_app_state());
    let session_id = "task-plan-enabled-session".to_string();
    let workspace = temp_workspace("task-plan-enabled");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("change the runtime loop".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::Execute;

    prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared");

    assert!(phase_state.task_plan.is_some());
    let prompt = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .and_then(|session| session.messages.first())
            .and_then(|message| message.content.clone())
            .expect("system prompt should be present")
    };
    assert!(prompt.contains("## Task Plan"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn prepare_analyze_snapshot_skips_task_plan_when_config_disabled() {
    let mut config = test_config();
    config.enable_task_plan = false;
    let state = Arc::new(test_app_state_with_config(config));
    let session_id = "task-plan-config-disabled-session".to_string();
    let workspace = temp_workspace("task-plan-config-disabled");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("change the runtime loop".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::Execute;

    prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared");

    assert!(phase_state.task_plan.is_none());
    let prompt = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .and_then(|session| session.messages.first())
            .and_then(|message| message.content.clone())
            .expect("system prompt should be present")
    };
    assert!(!prompt.contains("## Task Plan"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn build_plan_only_tools_includes_only_read_only_builtins() {
    let state = Arc::new(test_app_state());
    let config = state.config();
    let workspace = temp_workspace("plan-only-tools");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let tools = build_plan_only_tools(&config, Provider::OpenAI, &workspace).await;
    let names = tools
        .iter()
        .filter_map(tool_definition_name)
        .collect::<Vec<_>>();

    assert!(names.contains(&crate::tools::TOOL_NAME_THINK));
    assert!(names.contains(&crate::tools::TOOL_NAME_READ_FILE));
    assert!(names.contains(&crate::tools::TOOL_NAME_LIST_DIR));
    assert!(names.contains(&crate::tools::TOOL_NAME_SEARCH_FILES));
    assert!(names.contains(&crate::tools::TOOL_NAME_HTTP_FETCH));
    assert!(!names.contains(&crate::tools::TOOL_NAME_EXEC));
    assert!(!names.contains(&crate::tools::TOOL_NAME_WRITE_FILE));
    assert!(!names.contains(&crate::tools::TOOL_NAME_PATCH_FILE));
    assert!(!names.contains(&crate::tools::TOOL_NAME_DELETE_FILE));
    assert!(!names.contains(&crate::tools::TOOL_NAME_TODOS));
    assert!(!names.contains(&crate::tools::TOOL_NAME_VIEW_IMAGE));

    let unavailable_plan_names = available_tool_names_for_plan(&config, &workspace, false);
    let available_plan_names = available_tool_names_for_plan(&config, &workspace, true);
    let unavailable_read_only_names =
        available_tool_names_for_plan_only(&config, &workspace, false);
    let available_read_only_names = available_tool_names_for_plan_only(&config, &workspace, true);
    assert!(
        !unavailable_plan_names
            .iter()
            .any(|name| name == crate::tools::TOOL_NAME_VIEW_IMAGE)
    );
    assert!(
        available_plan_names
            .iter()
            .any(|name| name == crate::tools::TOOL_NAME_VIEW_IMAGE)
    );
    assert!(
        !unavailable_read_only_names
            .iter()
            .any(|name| name == crate::tools::TOOL_NAME_VIEW_IMAGE)
    );
    assert!(
        available_read_only_names
            .iter()
            .any(|name| name == crate::tools::TOOL_NAME_VIEW_IMAGE)
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn session_control_tool_is_only_exposed_for_main_execute_tools() {
    let state = Arc::new(test_app_state());
    let config = state.config();
    let workspace = temp_workspace("session-control-tools");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let main_tools = build_runtime_tools(&config, Provider::OpenAI, &workspace, MAIN_SESSION_ID)
        .await
        .into_iter()
        .filter_map(|tool| tool_definition_name(&tool).map(str::to_string))
        .collect::<Vec<_>>();
    let other_tools = build_runtime_tools(&config, Provider::OpenAI, &workspace, "worker")
        .await
        .into_iter()
        .filter_map(|tool| tool_definition_name(&tool).map(str::to_string))
        .collect::<Vec<_>>();
    let plan_tools = build_plan_only_tools(&config, Provider::OpenAI, &workspace)
        .await
        .into_iter()
        .filter_map(|tool| tool_definition_name(&tool).map(str::to_string))
        .collect::<Vec<_>>();

    assert!(main_tools.contains(&crate::tools::TOOL_NAME_SESSION_CONTROL.to_string()));
    assert!(!other_tools.contains(&crate::tools::TOOL_NAME_SESSION_CONTROL.to_string()));
    assert!(!plan_tools.contains(&crate::tools::TOOL_NAME_SESSION_CONTROL.to_string()));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn runtime_tool_catalog_uses_the_live_group_feature_flag() {
    let state = Arc::new(test_app_state());
    let config = state.config();
    assert!(config.enable_groups, "the test run snapshot enables Groups");
    let workspace = temp_workspace("live-group-feature-tools");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let definitions = build_runtime_tools_for_working_directory(
        &config,
        Provider::OpenAI,
        &workspace,
        &workspace,
        MAIN_SESSION_ID,
        false,
    )
    .await;
    let session_control = definitions
        .iter()
        .find(|definition| {
            tool_definition_name(definition) == Some(crate::tools::TOOL_NAME_SESSION_CONTROL)
        })
        .expect("main execute tools should include session_control")
        .to_string();

    assert!(!session_control.contains("create_group"));
    assert!(!session_control.contains("list_groups"));
    assert!(!session_control.contains("group_id"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn session_control_tool_rejects_non_main_session_even_if_called() {
    let state = Arc::new(test_app_state());

    let outcome = crate::session_control::execute_session_control_tool(
        &state,
        "worker",
        r#"{"action":"list_sessions"}"#,
    )
    .await;

    assert!(outcome.is_error);
    assert!(
        outcome
            .output
            .contains("only available in the main session")
    );
}

#[tokio::test]
async fn session_control_tool_rejects_dispatch_to_main_session() {
    let state = Arc::new(test_app_state());

    let outcome = crate::session_control::execute_session_control_tool(
        &state,
        MAIN_SESSION_ID,
        r#"{"action":"dispatch","targets":[" Main "],"message":"review this","wait":true}"#,
    )
    .await;

    assert!(outcome.is_error);
    assert!(
        outcome
            .output
            .contains("cannot dispatch to the main session")
    );
}

#[tokio::test]
async fn release_agent_run_for_stop_requested_requires_matching_token() {
    let state = Arc::new(test_app_state());
    let cancel = CancellationToken::new();
    let matching_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let other_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reservation = try_reserve_agent_run(&state, "worker-a", 42, &cancel, &matching_stop)
        .await
        .expect("run should reserve");

    assert!(
        !release_agent_run_for_stop_requested(&state, "worker-a", &other_stop).await,
        "non-matching stop token must not remove an active run"
    );
    assert!(state.active_runs.lock().await.contains_key("worker-a"));
    assert!(!reservation.run_cancel.is_cancelled());

    assert!(
        release_agent_run_for_stop_requested(&state, "worker-a", &matching_stop).await,
        "matching stop token should remove the active run"
    );
    assert!(!state.active_runs.lock().await.contains_key("worker-a"));
    assert!(reservation.run_cancel.is_cancelled());
}

#[tokio::test]
async fn execute_tool_call_rejects_session_control_in_plan_only_mode() {
    let state = Arc::new(test_app_state());
    let workspace = temp_workspace("plan-only-session-control");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: MAIN_SESSION_ID,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;
    phase_state.cycle_workspace = workspace.clone();
    let tc = ToolCall {
        id: "call-session-control".into(),
        call_type: "function".into(),
        gemini_thought_signature: None,
        function: FunctionCall {
            name: crate::tools::TOOL_NAME_SESSION_CONTROL.into(),
            arguments: r#"{"action":"create_session","name":"Should Not Create"}"#.into(),
        },
    };

    let image_budget =
        tools::mcp::ToolImageBudget::new(crate::image_uploads::MAX_IMAGE_UPLOAD_FILES);
    let (outcome, effective_args, _evidence_before) =
        execute_tool_call(&ctx, &mut phase_state, &tc, &image_budget)
            .await
            .expect("plan-only session_control rejection should be recorded");

    assert!(outcome.is_error);
    assert!(outcome.output.contains("rejected by plan mode"));
    assert!(effective_args.is_none());
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn plan_evidence_capture_honors_run_cancellation() {
    let workspace = temp_workspace("plan-evidence-cancel");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    std::fs::write(workspace.join("notes.txt"), "evidence")
        .expect("evidence fixture should be written");
    let run_cancel = CancellationToken::new();
    run_cancel.cancel();

    let capture = tokio::time::timeout(
        Duration::from_millis(100),
        capture_plan_tool_evidence(
            AgentRunMode::PlanOnly,
            crate::tools::TOOL_NAME_READ_FILE,
            r#"{"path":"notes.txt"}"#,
            &workspace,
            Duration::from_secs(30),
            None,
            &run_cancel,
        ),
    )
    .await
    .expect("a cancelled run must not wait for evidence collection");

    assert!(capture.failed);
    assert!(capture.evidence.is_empty());
    assert!(capture.deadline.is_some());
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn plan_evidence_capture_honors_the_shared_tool_deadline() {
    let workspace = temp_workspace("plan-evidence-deadline");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    std::fs::write(workspace.join("notes.txt"), "evidence")
        .expect("evidence fixture should be written");
    let run_cancel = CancellationToken::new();
    let expired_deadline = std::time::Instant::now()
        .checked_sub(Duration::from_millis(1))
        .expect("the test deadline should be representable");

    let capture = capture_plan_tool_evidence(
        AgentRunMode::PlanOnly,
        crate::tools::TOOL_NAME_READ_FILE,
        r#"{"path":"notes.txt"}"#,
        &workspace,
        Duration::from_secs(30),
        Some(expired_deadline),
        &run_cancel,
    )
    .await;

    assert!(capture.failed);
    assert!(capture.evidence.is_empty());
    assert_eq!(
        capture.remaining_timeout(Some(Duration::from_secs(30))),
        Some(Duration::ZERO)
    );
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn parallel_tool_timeout_pauses_while_waiting_for_ordered_image_budget() {
    let image_budget = tools::mcp::ToolImageBudget::new(1);
    let first = image_budget.for_call(0);
    let second = image_budget.for_call(1);
    let image_wait = second.subscribe_waiting();
    let release_first = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(80)).await;
        drop(first);
    });
    let future = async move {
        second.wait_for_turn().await;
        tools::ToolOutcome {
            output: "completed after ordered image wait".into(),
            is_error: false,
            duration_ms: 0,
            subagent_snapshot: None,
            images: Vec::new(),
        }
    };
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let cancel = CancellationToken::new();

    let result = tokio::time::timeout(
        Duration::from_secs(1),
        run_tool_with_feedback_with_image_wait(
            &live_tx,
            &cancel,
            "call-2",
            tools::TOOL_NAME_VIEW_IMAGE,
            Some(Duration::from_millis(30)),
            image_wait,
            future,
        ),
    )
    .await
    .expect("ordered budget wait should not consume the tool timeout");
    release_first.await.expect("release task should finish");

    match result {
        ToolRunState::Completed(outcome) => {
            assert!(!outcome.is_error);
            assert_eq!(outcome.output, "completed after ordered image wait");
        }
        ToolRunState::Abort => panic!("tool should not be cancelled"),
    }
}

#[tokio::test]
async fn run_act_phase_rejects_unavailable_view_image_in_parallel_batch() {
    let state = Arc::new(test_app_state());
    let workspace = temp_workspace("parallel-view-image-unavailable");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    std::fs::write(workspace.join("note.txt"), "parallel read succeeded")
        .expect("fixture should be written");
    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: MAIN_SESSION_ID,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.cycle_workspace = workspace.clone();
    phase_state.react_ctx.transition_to_act();
    phase_state.pending_tool_calls = vec![
        ToolCall {
            id: "call-view-image".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: crate::tools::TOOL_NAME_VIEW_IMAGE.into(),
                arguments: r#"{"path":"missing.png"}"#.into(),
            },
        },
        ToolCall {
            id: "call-read-file".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: crate::tools::TOOL_NAME_READ_FILE.into(),
                arguments: r#"{"path":"note.txt"}"#.into(),
            },
        },
    ];

    assert!(matches!(
        run_act_phase(&ctx, &mut phase_state).await,
        AgentPhaseControl::Continue
    ));
    assert_eq!(phase_state.collected_results.len(), 2);
    assert!(phase_state.collected_results[0].is_error);
    assert!(
        phase_state.collected_results[0]
            .result
            .contains("requires an image-capable model")
    );
    assert!(
        phase_state.collected_results[1]
            .result
            .contains("parallel read succeeded")
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

fn plan_only_mcp_test_config() -> Config {
    let mut config = test_config();
    config.mcp_servers.insert(
        "mock".to_string(),
        JsonMcpServerConfig {
            transport: None,
            command: "mock-mcp".to_string(),
            url: None,
            args: Vec::new(),
            env: HashMap::new(),
            headers: HashMap::new(),
            cwd: None,
            enabled: true,
            auth: None,
            timeout_secs: None,
        },
    );
    config
}

fn cached_read_only_mcp_descriptor(exposed_name: &str) -> crate::tools::mcp::McpToolDescriptor {
    crate::tools::mcp::McpToolDescriptor {
        server_name: "mock".to_string(),
        raw_name: "search".to_string(),
        exposed_name: exposed_name.to_string(),
        description: "Search repository metadata".to_string(),
        input_schema: json!({"type": "object", "properties": {}}),
        annotations: crate::tools::mcp::McpToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
        },
    }
}

#[tokio::test]
async fn plan_only_allowed_tool_requires_enabled_mcp_policy_tool() {
    let _guard = crate::tools::mcp::acquire_mcp_test_guard().await;
    let config = plan_only_mcp_test_config();
    let workspace = temp_workspace("plan-only-mcp-policy-deny");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let exposed_name = "mcp__mock__search__abc123";
    crate::tools::mcp::insert_cached_tool_descriptors_for_test(
        "mock",
        &config,
        &workspace,
        vec![cached_read_only_mcp_descriptor(exposed_name)],
    );
    crate::tools::mcp::save_session_policy(
        &workspace,
        &crate::tools::mcp::McpSessionPolicy {
            enabled_servers: HashSet::from(["mock".to_string()]),
            enabled_tools: HashSet::new(),
            ..Default::default()
        },
    )
    .expect("policy should save");

    assert!(
        crate::tools::mcp::is_read_only_tool_name(exposed_name, &config, &workspace),
        "global cached descriptor is read-only"
    );
    assert!(
        !is_plan_only_allowed_tool(exposed_name, &config, &workspace, &workspace),
        "PlanOnly must reject MCP tools not enabled by the session policy"
    );

    crate::tools::mcp::clear_cached_runtime_state_for_server("mock");
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn plan_only_allowed_tool_accepts_policy_enabled_read_only_mcp_tool() {
    let _guard = crate::tools::mcp::acquire_mcp_test_guard().await;
    let config = plan_only_mcp_test_config();
    let session_home = temp_workspace("plan-only-mcp-policy-allow-home");
    let working_directory = temp_workspace("plan-only-mcp-policy-allow-project");
    std::fs::create_dir_all(&session_home).expect("session home should be created");
    std::fs::create_dir_all(&working_directory).expect("working directory should be created");
    let exposed_name = "mcp__mock__search__def456";
    crate::tools::mcp::save_session_policy(
        &session_home,
        &crate::tools::mcp::McpSessionPolicy {
            enabled_servers: HashSet::from(["mock".to_string()]),
            enabled_tools: HashSet::from([exposed_name.to_string()]),
            ..Default::default()
        },
    )
    .expect("policy should save");
    let policy = crate::tools::mcp::load_session_policy(&session_home);
    crate::tools::mcp::insert_cached_tool_descriptors_for_policy_for_test(
        "mock",
        &config,
        &working_directory,
        &policy,
        vec![cached_read_only_mcp_descriptor(exposed_name)],
    );

    assert!(is_plan_only_allowed_tool(
        exposed_name,
        &config,
        &session_home,
        &working_directory
    ));

    crate::tools::mcp::clear_cached_runtime_state_for_server("mock");
    let _ = std::fs::remove_dir_all(&session_home);
    let _ = std::fs::remove_dir_all(&working_directory);
}

#[tokio::test]
async fn prepare_analyze_snapshot_keeps_plan_only_prompt_read_only() {
    let state = Arc::new(test_app_state());
    let session_id = "plan-only-prompt-session".to_string();
    let workspace = temp_workspace("plan-only-prompt");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("plan the runtime loop changes".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;

    prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared");

    let prompt = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .and_then(|session| session.messages.first())
            .and_then(|message| message.content.clone())
            .expect("system prompt should be present")
    };
    assert!(prompt.contains("plan-only mode"));
    assert!(prompt.contains("Do not write files"));
    assert!(prompt.contains("`submit_plan`"));
    assert!(prompt.contains("**think**"));
    assert!(prompt.contains("**read_file**"));
    assert!(prompt.contains("**list_dir**"));
    assert!(prompt.contains("**search_files**"));
    assert!(prompt.contains("**http_fetch**"));
    assert!(!prompt.contains("**exec**"));
    assert!(!prompt.contains("**write_file**"));
    assert!(!prompt.contains("**patch_file**"));
    assert!(!prompt.contains("**delete_file**"));
    assert!(!prompt.contains("**todos**"));
    assert!(!prompt.contains("use the `todos` tool"));
    assert!(!prompt.contains("Other available tools"));
    assert!(!prompt.contains("- **Delegation:**"));
    assert!(!prompt.contains("## Delegation Guidance"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn execute_tool_call_rejects_mutating_tools_in_plan_only_mode() {
    let state = Arc::new(test_app_state());
    let session_id = "plan-only-reject-tool".to_string();
    let workspace = temp_workspace("plan-only-reject-tool");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;
    phase_state.cycle_workspace = workspace.clone();
    let tc = ToolCall {
        id: "call-write".into(),
        call_type: "function".into(),
        gemini_thought_signature: None,
        function: FunctionCall {
            name: crate::tools::TOOL_NAME_WRITE_FILE.into(),
            arguments: r#"{"path":"demo.txt","content":"changed"}"#.into(),
        },
    };

    let image_budget =
        tools::mcp::ToolImageBudget::new(crate::image_uploads::MAX_IMAGE_UPLOAD_FILES);
    let (outcome, effective_args, _evidence_before) =
        execute_tool_call(&ctx, &mut phase_state, &tc, &image_budget)
            .await
            .expect("plan-only rejection should be recorded as a tool outcome");

    assert!(outcome.is_error);
    assert!(outcome.output.contains("rejected by plan mode"));
    assert!(effective_args.is_none());
    assert!(!workspace.join("demo.txt").exists());
    let event = live_rx
        .recv()
        .await
        .expect("tool call event should be emitted");
    assert_eq!(event["type"].as_str(), Some("tool_call"));
    assert_eq!(
        event["name"].as_str(),
        Some(crate::tools::TOOL_NAME_WRITE_FILE)
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn refreshed_revision_replaces_evidence_only_when_submission_succeeds() {
    let state = Arc::new(test_app_state());
    let session_id = "plan-refresh-replacement".to_string();
    let workspace = temp_workspace("plan-refresh-replacement");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    std::fs::write(workspace.join("old.txt"), "old").expect("old fixture should write");
    std::fs::write(workspace.join("new.txt"), "new").expect("new fixture should write");
    let old_evidence = crate::plan::capture_tool_evidence(
        crate::tools::TOOL_NAME_READ_FILE,
        r#"{"path":"old.txt"}"#,
        &workspace,
    );
    let new_evidence = crate::plan::capture_tool_evidence(
        crate::tools::TOOL_NAME_READ_FILE,
        r#"{"path":"new.txt"}"#,
        &workspace,
    );
    let mut session = test_session(&session_id, "Plan Refresh Replacement", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Refresh the plan".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_refresh_replacement".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        revision: 2,
        status: crate::plan::PlanStatus::Planning,
        evidence: old_evidence,
        evidence_truncated: true,
        created_at: 10,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;
    phase_state.replace_plan_evidence = true;
    phase_state.plan_evidence = new_evidence.clone();
    phase_state.plan_submission = Some(
        crate::plan::validate_submission_json(
            r#"{
                "state":"ready",
                "title":"Refreshed plan",
                "goal":"Use current evidence",
                "steps":[{"id":"verify","title":"Verify current state"}],
                "acceptance_criteria":["The approved workspace remains available."],
                "completion_checks":[{"id":"verify-workspace","step_id":"verify","covers":[{"section":"acceptance_criteria","index":0}],"kind":"workspace_path","path":".","expected_path_type":"directory"}]
            }"#,
        )
        .expect("submission should validate"),
    );
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let events = {
        let mut sessions = state.sessions.lock().await;
        register_pending_plan(
            sessions
                .get_mut(&session_id)
                .expect("planning Session should remain loaded"),
            &mut phase_state,
        )
    };

    assert!(!events.is_empty());
    let sessions = state.sessions.lock().await;
    let plan = sessions[&session_id]
        .pending_plan
        .as_ref()
        .expect("refreshed plan should be registered");
    assert_eq!(plan.revision, 3);
    assert_eq!(plan.evidence, new_evidence);
    assert!(!plan.evidence_truncated);
    drop(sessions);
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn pruned_plan_message_anchors_do_not_reuse_an_existing_revision() {
    let state = Arc::new(test_app_state());
    let session_id = "plan-pruned-revision".to_string();
    let workspace = temp_workspace("plan-pruned-revision");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let mut session = test_session(&session_id, "Pruned Plan Revision", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Revise the pruned plan".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_pruned_revision".into(),
        original_user_message_index: 0,
        assistant_plan_message_index: 0,
        revision: 2,
        status: crate::plan::PlanStatus::Planning,
        artifact: crate::plan::PlanArtifact {
            schema_version: 1,
            title: "Existing revision".into(),
            goal: "Preserve optimistic concurrency".into(),
            steps: vec![crate::plan::PlanStep {
                id: "inspect".into(),
                title: "Inspect the state".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        initial_submission_pending: false,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;
    phase_state.plan_submission = Some(
        crate::plan::validate_submission_json(
            r#"{
                "state":"ready",
                "title":"Revised plan",
                "goal":"Preserve optimistic concurrency",
                "steps":[{"id":"implement","title":"Implement safely"}],
                "acceptance_criteria":["The approved workspace remains available."],
                "completion_checks":[{"id":"implement-workspace","step_id":"implement","covers":[{"section":"acceptance_criteria","index":0}],"kind":"workspace_path","path":".","expected_path_type":"directory"}]
            }"#,
        )
        .expect("submission should validate"),
    );
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let events = {
        let mut sessions = state.sessions.lock().await;
        register_pending_plan(
            sessions
                .get_mut(&session_id)
                .expect("planning Session should remain loaded"),
            &mut phase_state,
        )
    };

    assert!(!events.is_empty());
    let sessions = state.sessions.lock().await;
    let plan = sessions[&session_id]
        .pending_plan
        .as_ref()
        .expect("revised plan should be registered");
    assert_eq!(plan.revision, 3);
    assert!(!plan.initial_submission_pending);
    drop(sessions);
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn plan_tool_calling_fallback_removes_tools_and_adds_text_plan_contract() {
    let mut messages = vec![ChatMessage {
        role: "system".into(),
        content: Some("Base system prompt".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let mut tools = vec![serde_json::json!({"type":"function","name":"submit_plan"})];

    assert!(apply_plan_tool_calling_fallback(&mut messages, &mut tools));
    assert!(tools.is_empty());
    assert!(
        messages[0]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("exactly one concrete implementation plan"))
    );
    assert!(!apply_plan_tool_calling_fallback(&mut messages, &mut tools));
    let event = plan_tool_calling_fallback_event();
    assert_eq!(event["type"], "progress");
    assert_eq!(event["code"], "plan_tool_calling_fallback");
}

#[test]
fn plan_only_plain_response_activates_legacy_fallback_once() {
    let plain_response = ChatMessage {
        role: "assistant".into(),
        content: Some("A concrete Markdown plan".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;

    assert!(activate_plan_text_fallback_for_plain_response(
        &mut phase_state,
        &plain_response
    ));
    assert!(phase_state.plan_text_fallback_used);
    assert!(!activate_plan_text_fallback_for_plain_response(
        &mut phase_state,
        &plain_response
    ));

    let mut execute_state = phase_state_for_analyze_test();
    assert!(!activate_plan_text_fallback_for_plain_response(
        &mut execute_state,
        &plain_response
    ));

    let mut tool_response = plain_response;
    tool_response.tool_calls = Some(vec![crate::ToolCall {
        id: "submit-plan".into(),
        call_type: "function".into(),
        gemini_thought_signature: None,
        function: crate::FunctionCall {
            name: crate::plan::TOOL_NAME_SUBMIT_PLAN.into(),
            arguments: "{}".into(),
        },
    }]);
    let mut tool_state = phase_state_for_analyze_test();
    tool_state.run_mode = AgentRunMode::PlanOnly;
    assert!(!activate_plan_text_fallback_for_plain_response(
        &mut tool_state,
        &tool_response
    ));

    let mut proven_tool_state = phase_state_for_analyze_test();
    proven_tool_state.run_mode = AgentRunMode::PlanOnly;
    proven_tool_state.react_ctx.tool_calls = 1;
    assert!(!activate_plan_text_fallback_for_plain_response(
        &mut proven_tool_state,
        &ChatMessage {
            tool_calls: None,
            ..tool_response
        }
    ));
    assert!(!proven_tool_state.plan_text_fallback_used);
}

#[tokio::test]
async fn run_finish_phase_plan_only_compatibility_fallback_registers_legacy_plan() {
    let mut config = test_config();
    config.structured_memory = true;
    let state = Arc::new(test_app_state_with_config(config.clone()));
    let queue = crate::memory::MemoryUpdateQueue::spawn(
        config,
        state.sessions.clone(),
        state.auxiliary_tasks.clone(),
    );
    {
        let mut guard = state.memory_queue.lock().expect("memory queue lock");
        *guard = Some(queue.clone());
    }

    let session_id = "plan-only-finish-memory".to_string();
    let workspace = temp_workspace("plan-only-finish-memory");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let mut session = test_session(&session_id, "Plan Finish", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("plan the next change".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: Some("Goal: inspect and propose the implementation steps.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;
    phase_state.plan_text_fallback_used = true;
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let control = run_finish_phase(&ctx, &mut phase_state).await;

    assert!(matches!(control, AgentPhaseControl::Break));
    assert_eq!(queue.status_snapshot().enqueued, 0);
    {
        let sessions = state.sessions.lock().await;
        assert!(
            sessions
                .get(&session_id)
                .and_then(|session| session.pending_plan.as_ref())
                .is_some_and(|plan| plan.assistant_plan_message_index == 2)
        );
    }
    let plan_ready = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while let Some(event) = live_rx.recv().await {
            if event["type"].as_str() == Some("plan_ready") {
                return Some(event);
            }
        }
        None
    })
    .await
    .expect("plan_ready event should arrive")
    .expect("plan_ready event should be present");
    assert_eq!(plan_ready["message_index"].as_u64(), Some(2));
    assert!(
        plan_ready["plan_id"]
            .as_str()
            .is_some_and(|plan_id| plan_id.starts_with("plan_"))
    );

    queue.shutdown();
    let _ = std::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    );
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn run_finish_phase_plan_only_plain_response_registers_legacy_plan() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "plan-only-unsubmitted-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("plan-only-unsubmitted");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let mut session = test_session(&session_id, "Unsubmitted Plan", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("plan the next change".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    let plain_response = ChatMessage {
        role: "assistant".into(),
        content: Some("Goal: inspect and propose the implementation steps.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    session.messages.push(plain_response.clone());
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_unsubmitted".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 1,
        revision: 1,
        status: crate::plan::PlanStatus::Planning,
        artifact: crate::plan::initial_placeholder_artifact("plan the next change", false)
            .expect("placeholder should validate"),
        created_at: 10,
        updated_at: 10,
        initial_submission_pending: true,
        ..Default::default()
    });
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;
    assert!(activate_plan_text_fallback_for_plain_response(
        &mut phase_state,
        &plain_response
    ));
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let control = run_finish_phase(&ctx, &mut phase_state).await;

    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(!phase_state.run_failed);
    let sessions = state.sessions.lock().await;
    let plan = sessions[&session_id]
        .pending_plan
        .as_ref()
        .expect("the plain response should become a legacy plan");
    assert_eq!(plan.id, "plan_unsubmitted");
    assert_eq!(plan.status, crate::plan::PlanStatus::Ready);
    assert_eq!(
        plan.artifact.legacy_markdown.as_deref(),
        plain_response.content.as_deref()
    );
    drop(sessions);
    assert!(
        std::iter::from_fn(|| live_rx.try_recv().ok()).any(|event| event["type"] == "plan_ready")
    );

    crate::session_store::delete_session_from_storage(&session_id)
        .await
        .expect("test session should be removed");
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn structured_plan_reuses_the_submit_plan_assistant_message() {
    let state = Arc::new(test_app_state());
    let session_id = "structured-plan-message-anchor".to_string();
    let workspace = temp_workspace("structured-plan-message-anchor");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let mut session = test_session(&session_id, "Structured Plan Anchor", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Plan the change".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: Some("I prepared a plan.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "submit-plan-call".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: crate::plan::TOOL_NAME_SUBMIT_PLAN.into(),
                arguments: "{}".into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "tool".into(),
        content: Some("Plan accepted.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: Some("submit-plan-call".into()),
        timestamp: None,
    });
    let original_message_count = session.messages.len();
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;
    phase_state.plan_submission = Some(
        crate::plan::validate_submission_json(
            r#"{
                "state":"ready",
                "title":"Canonical plan",
                "goal":"Keep one visible plan representation",
                "steps":[{"id":"implement","title":"Implement the change"}],
                "acceptance_criteria":["The approved workspace remains available."],
                "completion_checks":[{"id":"implement-workspace","step_id":"implement","covers":[{"section":"acceptance_criteria","index":0}],"kind":"workspace_path","path":".","expected_path_type":"directory"}]
            }"#,
        )
        .expect("submission should validate"),
    );
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let events = {
        let mut sessions = state.sessions.lock().await;
        register_pending_plan(
            sessions
                .get_mut(&session_id)
                .expect("planning Session should remain loaded"),
            &mut phase_state,
        )
    };

    assert!(!events.is_empty());
    let sessions = state.sessions.lock().await;
    let session = &sessions[&session_id];
    let plan = session
        .pending_plan
        .as_ref()
        .expect("plan should be registered");
    assert_eq!(session.messages.len(), original_message_count);
    assert_eq!(plan.assistant_plan_message_index, 2);
    assert!(
        session.messages[2]
            .content
            .as_deref()
            .is_some_and(|content| content.contains("# Canonical plan"))
    );
    assert_eq!(
        session.messages[2].tool_calls.as_ref().map(Vec::len),
        Some(1)
    );
    drop(sessions);
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn run_finish_phase_plan_only_empty_response_does_not_register_old_plan() {
    let state = Arc::new(test_app_state());
    let session_id = "plan-only-empty-finish".to_string();
    let workspace = temp_workspace("plan-only-empty-finish");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let mut session = test_session(&session_id, "Plan Empty Finish", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("first request".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: Some("Old assistant response".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("plan the new change".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Empty);

    let control = run_finish_phase(&ctx, &mut phase_state).await;

    assert!(matches!(control, AgentPhaseControl::Break));
    {
        let sessions = state.sessions.lock().await;
        assert!(
            sessions
                .get(&session_id)
                .expect("session should exist")
                .pending_plan
                .is_none()
        );
    }
    let done = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while let Some(event) = live_rx.recv().await {
            assert_ne!(event["type"].as_str(), Some("plan_ready"));
            if event["type"].as_str() == Some("done") {
                return event;
            }
        }
        serde_json::Value::Null
    })
    .await
    .expect("done event should arrive");
    assert_eq!(done["reason"].as_str(), Some("empty"));

    let _ = std::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    );
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn run_finish_phase_does_not_report_completion_when_persistence_fails() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "finish-persist-failure-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Finish Persist Failure", None);
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("finish the persisted plan".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: Some("completed response".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    let executing_plan = crate::PendingPlan {
        id: "plan_finish_persist_failure".into(),
        revision: 2,
        status: crate::plan::PlanStatus::Executing,
        approved_at: Some(20),
        execution_attempt: 1,
        created_at: 10,
        updated_at: 20,
        ..Default::default()
    };
    session.pending_plan = Some(executing_plan.clone());
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let failure_path = crate::session_store::sessions_dir().join(format!("{session_id}.json.tmp"));
    std::fs::create_dir_all(&failure_path).expect("failure sentinel directory should be created");

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.approved_plan = Some(executing_plan);
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let control = run_finish_phase(&ctx, &mut phase_state).await;

    std::fs::remove_dir_all(&failure_path).expect("failure sentinel should be removed");
    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(phase_state.run_failed);
    let sessions = state.sessions.lock().await;
    let plan = sessions[&session_id]
        .pending_plan
        .as_ref()
        .expect("the approved plan should remain available after rollback");
    assert_eq!(plan.status, crate::plan::PlanStatus::Executing);
    assert!(plan.finished_at.is_none());
    drop(sessions);
    let error = live_rx
        .try_recv()
        .expect("persistence failure should be reported");
    assert_eq!(error["type"].as_str(), Some("error"));
    assert!(
        error["content"]
            .as_str()
            .is_some_and(|content| content.contains("could not be saved"))
    );
    assert!(
        live_rx.try_recv().is_err(),
        "a failed final save must not emit done or finish hooks"
    );
}

#[tokio::test]
async fn run_finish_phase_marks_unreported_execution_steps_failed() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "finish-unreported-plan-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Unreported execution", None);
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("execute every approved plan step".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: Some("The plan is complete.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    let executing_plan = crate::PendingPlan {
        id: "plan_unreported_execution".into(),
        revision: 3,
        status: crate::plan::PlanStatus::Executing,
        artifact: crate::plan::PlanArtifact {
            title: "Create an artifact".into(),
            goal: "Create result.txt".into(),
            steps: vec![crate::plan::PlanStep {
                id: "write-result".into(),
                title: "Write result.txt".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        progress: vec![crate::plan::PlanProgressStep {
            id: "write-result".into(),
            title: "Write result.txt".into(),
            status: crate::plan::PlanStepStatus::Pending,
            ..Default::default()
        }],
        approved_at: Some(20),
        execution_attempt: 1,
        created_at: 10,
        updated_at: 20,
        ..Default::default()
    };
    session.pending_plan = Some(executing_plan.clone());
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.approved_plan = Some(executing_plan);
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let control = run_finish_phase(&ctx, &mut phase_state).await;

    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(phase_state.run_failed);
    let plan = state.sessions.lock().await[&session_id]
        .pending_plan
        .clone()
        .expect("failed approved plan should remain resumable");
    assert_eq!(plan.status, crate::plan::PlanStatus::Failed);
    assert_eq!(plan.unfinished_step_count(), 1);
    assert!(plan.finished_at.is_some());
    assert_eq!(
        plan.to_live_value()["run_finished_with_unreported_steps"],
        false
    );

    let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        events
            .iter()
            .any(|event| { event["type"] == "plan_state" && event["plan"]["status"] == "failed" })
    );
    assert!(
        events.iter().any(|event| {
            event["type"] == "error" && event["code"] == "plan_execution_incomplete"
        })
    );
    assert!(events.iter().any(|event| {
        event["type"] == "done"
            && event["phase"] == "failed"
            && event["reason"] == "incomplete_plan"
    }));

    crate::session_store::delete_session_from_storage(&session_id)
        .await
        .expect("test session should be removed");
}

#[tokio::test]
async fn finish_missing_or_ambiguous_anchor_emits_one_live_terminal_without_outcome() {
    for (suffix, messages) in [
        (
            "missing",
            vec![ChatMessage {
                role: "assistant".into(),
                content: Some("there is no originating user message".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(1),
            }],
        ),
        (
            "ambiguous",
            vec![
                ChatMessage {
                    role: "user".into(),
                    content: Some("identical run input".into()),
                    images: None,
                    thinking: None,
                    anthropic_thinking_blocks: None,
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: Some(2),
                },
                ChatMessage {
                    role: "user".into(),
                    content: Some("identical run input".into()),
                    images: None,
                    thinking: None,
                    anthropic_thinking_blocks: None,
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: Some(2),
                },
                ChatMessage {
                    role: "assistant".into(),
                    content: Some("ambiguous terminal result".into()),
                    images: None,
                    thinking: None,
                    anthropic_thinking_blocks: None,
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: Some(3),
                },
            ],
        ),
    ] {
        let state = Arc::new(test_app_state());
        let session_id = format!("terminal-anchor-live-{suffix}");
        let mut session = test_session(&session_id, "Terminal anchor", None);
        session.messages.extend(messages);
        state
            .sessions
            .lock()
            .await
            .insert(session_id.clone(), session);
        let cancel = CancellationToken::new();
        let run_cancel = CancellationToken::new();
        let (live_tx, mut live_rx) = mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
        let ctx = AgentRunCtx {
            state: &state,
            config: state.config(),
            model: state.config().model.clone(),
            current_session_id: &session_id,
            cancel: &cancel,
            live_tx: &live_tx,
            run_cancel: &run_cancel,
        };
        let mut phase_state = phase_state_for_analyze_test();
        if suffix == "ambiguous" {
            phase_state.terminal_message_anchor =
                RunMessageAnchor::from_messages(&state.sessions.lock().await[&session_id].messages)
                    .ok();
        }
        phase_state
            .react_ctx
            .transition_to_finish(agent::FinishReason::Complete);

        assert!(matches!(
            run_finish_phase(&ctx, &mut phase_state).await,
            AgentPhaseControl::Break
        ));
        assert!(phase_state.run_failed);
        let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
        let terminal = events
            .iter()
            .filter(|event| event["run_terminal"] == true)
            .collect::<Vec<_>>();
        assert_eq!(terminal.len(), 1, "{suffix} should emit one terminal");
        assert_eq!(terminal[0]["type"], "error");
        assert_eq!(terminal[0]["code"], "terminal_identity_unavailable");
        assert_eq!(terminal[0]["phase"], "incomplete");
        assert!(!events.iter().any(|event| event["type"] == "done"));
        assert!(
            take_terminal_persistence_record(&session_id)
                .await
                .is_none()
        );
    }
}

#[tokio::test]
async fn finish_concurrent_anchor_rewrite_emits_one_terminal_without_generic_duplicate() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "terminal-anchor-concurrent-{}",
        crate::generate_random_session_id().expect("random Session id")
    );
    let workspace = temp_workspace("terminal-anchor-concurrent");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should exist");
    let mut session = test_session(&session_id, "Concurrent terminal anchor", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.messages.extend([
        ChatMessage {
            role: "user".into(),
            content: Some("original terminal anchor".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(1),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("final response".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(2),
        },
    ]);
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);
    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx) = mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let gate = Arc::new(FinishPhaseTestGate::new(
        FinishPhaseTestStage::PostSessionMutation,
    ));
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.cycle_workspace = workspace;
    phase_state.completion_evidence.finish_gate = Some(Arc::clone(&gate));
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let rewrite = async {
        wait_for_finish_phase_gate(&gate).await;
        let _release = FinishPhaseGateRelease(Arc::clone(&gate));
        let mut sessions = state.sessions.lock().await;
        let user = sessions
            .get_mut(&session_id)
            .and_then(|session| {
                session
                    .messages
                    .iter_mut()
                    .find(|message| message.role == "user")
            })
            .expect("originating user message should exist");
        user.content = Some("concurrently rewritten anchor".into());
    };
    let (control, ()) = tokio::join!(run_finish_phase(&ctx, &mut phase_state), rewrite);

    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(phase_state.run_failed);
    let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
    let terminal = events
        .iter()
        .filter(|event| event["run_terminal"] == true)
        .collect::<Vec<_>>();
    assert_eq!(terminal.len(), 1, "anchor failure must have one terminal");
    assert_eq!(terminal[0]["code"], "terminal_identity_unavailable");
    assert_eq!(terminal[0]["phase"], "incomplete");
    assert!(!events.iter().any(|event| {
        event["content"] == "The final Agent state could not be saved." || event["type"] == "done"
    }));
    assert!(
        take_terminal_persistence_record(&session_id)
            .await
            .is_none()
    );
}

#[tokio::test]
async fn run_finish_phase_stop_cancels_a_slow_completion_verifier_without_terminal_write() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "finish-contract-stop-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("finish-contract-stop");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let plan = completion_verifier_test_plan("plan_finish_contract_stop");
    let mut session = test_session(&session_id, "Finish contract stop", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.pending_plan = Some(plan.clone());
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(false));
    state.active_runs.lock().await.insert(
        session_id.clone(),
        SessionRunBinding {
            connection_id: 81,
            cancel: run_cancel.clone(),
            stop_requested: Arc::clone(&stop_requested),
            deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
        },
    );
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let gate = Arc::new(CompletionVerifierTestGate::default());
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::Execute;
    phase_state.cycle_workspace = workspace.clone();
    phase_state.approved_plan = Some(plan);
    phase_state.completion_evidence.verifier_gate = Some(Arc::clone(&gate));
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let trigger = async {
        wait_for_completion_verifier_gate(&gate).await;
        stop_requested.store(true, Ordering::Relaxed);
    };
    let (control, ()) = tokio::join!(run_finish_phase(&ctx, &mut phase_state), trigger);

    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(phase_state.run_stopped);
    assert!(!phase_state.run_failed);
    assert!(run_cancel.is_cancelled());
    wait_for_completion_verifier_stop(&gate).await;
    let persisted_plan = state.sessions.lock().await[&session_id]
        .pending_plan
        .clone()
        .expect("executing plan should remain untouched for outer stop handling");
    assert_eq!(persisted_plan.status, crate::plan::PlanStatus::Executing);
    assert!(persisted_plan.finished_at.is_none());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(live_rx.try_recv().is_err());
    assert_eq!(
        state.sessions.lock().await[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Executing)
    );
    state.active_runs.lock().await.remove(&session_id);
}

#[tokio::test]
async fn run_finish_phase_stop_wins_at_every_post_verifier_blocking_boundary() {
    for (index, stage) in [
        FinishPhaseTestStage::OnFinishHook,
        FinishPhaseTestStage::Usage,
        FinishPhaseTestStage::BetweenHookEvents,
        FinishPhaseTestStage::BetweenPlanEvents,
        FinishPhaseTestStage::PostSessionMutation,
        FinishPhaseTestStage::PreTerminalCommit,
    ]
    .into_iter()
    .enumerate()
    {
        let state = Arc::new(test_app_state());
        let session_id = format!(
            "finish-post-verifier-stop-{index}-{}",
            crate::generate_random_session_id().expect("random session id")
        );
        let workspace = temp_workspace(&format!("finish-post-verifier-stop-{index}"));
        let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
        std::fs::create_dir_all(&workspace).expect("workspace should be created");
        let plan = completion_verifier_test_plan(&format!("plan_finish_stop_{index}"));
        let mut session = test_session(&session_id, "Finish post-verifier stop", None);
        session.workspace = workspace.clone();
        session.working_directory = workspace.clone();
        session.messages.push(ChatMessage {
            role: "user".into(),
            content: Some(format!("finish the approved plan at stage {stage:?}")),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(30 + index as u64),
        });
        session.pending_plan = Some(plan.clone());
        state
            .sessions
            .lock()
            .await
            .insert(session_id.clone(), session);

        let cancel = CancellationToken::new();
        let run_cancel = CancellationToken::new();
        let stop_requested = Arc::new(AtomicBool::new(false));
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 180 + index as u64,
                cancel: run_cancel.clone(),
                stop_requested: Arc::clone(&stop_requested),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
        let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
            mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
        let ctx = AgentRunCtx {
            state: &state,
            config: state.config(),
            model: state.config().model.clone(),
            current_session_id: &session_id,
            cancel: &cancel,
            live_tx: &live_tx,
            run_cancel: &run_cancel,
        };
        let gate = Arc::new(FinishPhaseTestGate::new(stage));
        let mut phase_state = phase_state_for_analyze_test();
        phase_state.run_mode = AgentRunMode::Execute;
        phase_state.cycle_workspace = workspace.clone();
        phase_state.approved_plan = Some(plan);
        phase_state.completion_evidence.finish_gate = Some(Arc::clone(&gate));
        phase_state
            .react_ctx
            .transition_to_finish(agent::FinishReason::Complete);

        let trigger = async {
            wait_for_finish_phase_gate(&gate).await;
            stop_requested.store(true, Ordering::Relaxed);
        };
        assert!(
            session_store::load_session_from_storage_result(&session_id)
                .expect("test storage lookup should succeed")
                .is_none()
        );
        let (control, ()) = tokio::join!(run_finish_phase(&ctx, &mut phase_state), trigger);

        assert!(matches!(control, AgentPhaseControl::Break), "{stage:?}");
        assert!(phase_state.run_stopped, "{stage:?}");
        assert!(!phase_state.run_failed, "{stage:?}");
        let current_plan = state.sessions.lock().await[&session_id]
            .pending_plan
            .clone()
            .expect("approved plan should remain available");
        assert_eq!(
            current_plan.status,
            crate::plan::PlanStatus::Executing,
            "{stage:?}"
        );
        assert!(current_plan.finished_at.is_none(), "{stage:?}");
        assert_eq!(
            phase_state.completion_evidence.terminal_linearization,
            TerminalLinearizationState::PreCommit,
            "{stage:?} must not choose a natural terminal transaction"
        );
        assert!(
            session_store::load_session_from_storage_result(&session_id)
                .expect("test storage lookup should succeed")
                .is_none(),
            "{stage:?} must not perform an independent pre-terminal Session save"
        );
        assert!(
            std::iter::from_fn(|| live_rx.try_recv().ok()).all(|event| {
                event["type"] != "done"
                    && event["type"] != "plan_state"
                    && !(event["type"] == "error" && event["run_terminal"] == true)
            }),
            "{stage:?} emitted a late terminal or Plan event"
        );
        state.active_runs.lock().await.remove(&session_id);
    }
}

#[tokio::test]
async fn run_finish_phase_natural_terminal_wins_after_database_commit_is_chosen() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "finish-post-commit-stop-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("finish-post-commit-stop");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let plan = completion_verifier_test_plan("plan_finish_post_commit_stop");
    let mut session = test_session(&session_id, "Finish post-commit stop", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("finish the approved plan before the late stop".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(40),
    });
    session.pending_plan = Some(plan.clone());
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(false));
    state.active_runs.lock().await.insert(
        session_id.clone(),
        SessionRunBinding {
            connection_id: 191,
            cancel: run_cancel.clone(),
            stop_requested: Arc::clone(&stop_requested),
            deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
        },
    );
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let gate = Arc::new(FinishPhaseTestGate::new(
        FinishPhaseTestStage::PostCommitBeforeAck,
    ));
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::Execute;
    phase_state.cycle_workspace = workspace.clone();
    phase_state.approved_plan = Some(plan);
    phase_state.completion_evidence.finish_gate = Some(Arc::clone(&gate));
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let trigger = async {
        wait_for_finish_phase_gate(&gate).await;
        assert_eq!(
            state.sessions.lock().await[&session_id]
                .pending_plan
                .as_ref()
                .map(|plan| plan.status),
            Some(crate::plan::PlanStatus::Executing),
            "in-memory readers must not observe Plan completion before commit acknowledgement"
        );
        assert!(live_rx.try_recv().is_err());
        stop_requested.store(true, Ordering::Relaxed);
        gate.release.store(true, Ordering::Relaxed);
    };
    let (control, ()) = tokio::join!(run_finish_phase(&ctx, &mut phase_state), trigger);

    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(!phase_state.run_stopped);
    assert!(!phase_state.run_failed);
    assert_eq!(
        phase_state.completion_evidence.terminal_linearization,
        TerminalLinearizationState::Committed
    );
    assert_eq!(
        state.sessions.lock().await[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Completed)
    );
    let (_, outcome) = take_terminal_persistence_record(&session_id)
        .await
        .expect("the natural terminal transaction should be recorded");
    assert_eq!(outcome.status, "completed");
    let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| {
            event["type"] == "plan_state" && event["plan"]["status"] == "completed"
        })
    );
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "done" && event["phase"] == "finish")
    );
    stop_requested.store(false, Ordering::Relaxed);
    state.active_runs.lock().await.remove(&session_id);
}

#[test]
fn terminal_session_patch_preserves_concurrent_non_run_fields_and_rejects_a_new_tail() {
    let session_id = "terminal-field-merge";
    let mut base = test_session(session_id, "Original name", None);
    base.messages.extend([
        ChatMessage {
            role: "user".into(),
            content: Some("finish this run".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(10),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("run result".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(11),
        },
    ]);
    base.pending_plan = Some(completion_verifier_test_plan("terminal-field-merge-plan"));

    let mut terminal = base.clone();
    let terminal_plan = terminal.pending_plan.as_mut().expect("plan should exist");
    terminal_plan.status = crate::plan::PlanStatus::Completed;
    terminal_plan.finished_at = Some(30);
    terminal.updated_at = 30;
    let anchor = RunMessageAnchor::from_messages(&base.messages)
        .expect("run user anchor should be captured");
    let patch = TerminalSessionPatch::new(&base, &terminal, &anchor)
        .expect("terminal patch should be constructible");

    let mut latest = base.clone();
    latest.name = "Renamed while Finish waited".into();
    latest.model_override = Some("provider/new-model".into());
    latest.think_level = "high".into();
    latest.input_tokens = 900;
    latest.todos = crate::todos::TodoSnapshot {
        revision: 4,
        items: vec![crate::todos::TodoItem {
            id: "keep".into(),
            content: "Keep concurrent Todo".into(),
            status: crate::todos::TodoStatus::InProgress,
        }],
        last_updated_by: crate::todos::TodoUpdatedBy::User,
        updated_at: 40,
    };
    latest.updated_at = 40;

    let merged = patch
        .apply_to_latest(&latest)
        .expect("unrelated concurrent fields should merge");
    assert_eq!(merged.name, "Renamed while Finish waited");
    assert_eq!(merged.model_override.as_deref(), Some("provider/new-model"));
    assert_eq!(merged.think_level, "high");
    assert_eq!(merged.input_tokens, 900);
    assert_eq!(merged.todos.revision, 4);
    assert_eq!(
        merged.pending_plan.as_ref().map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Completed)
    );
    assert!(terminal_patch_values_match(
        &merged.messages,
        &terminal.messages
    ));
    assert_eq!(merged.updated_at, 40);

    let mut next_run_started = latest;
    next_run_started.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("next run must not enter the old boundary".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(12),
    });
    assert!(
        patch.apply_to_latest(&next_run_started).is_err(),
        "a changed run-owned tail must fail closed instead of dropping or adopting it"
    );

    let mut different_plan_generation = next_run_started;
    different_plan_generation.messages.pop();
    different_plan_generation
        .pending_plan
        .as_mut()
        .expect("plan should still exist")
        .revision += 1;
    assert!(
        patch.apply_to_latest(&different_plan_generation).is_err(),
        "a different Plan revision must not be overwritten by an old terminal patch"
    );
}

#[test]
fn run_message_anchor_survives_prefix_compression_duplicates_and_image_url_normalization() {
    let older = ChatMessage {
        role: "user".into(),
        content: Some("repeat this request".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(100),
    };
    let current = ChatMessage {
        role: "user".into(),
        content: Some("repeat this request".into()),
        images: Some(vec![crate::ImageAttachment {
            url: "https://signed.example/temporary".into(),
            name: Some("request.png".into()),
            mime_type: Some("image/png".into()),
            s3_object_key: Some("uploads/request.png".into()),
            s3_config_id: Some("s3-current".into()),
            cache_path: None,
            data: None,
        }]),
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(200),
    };
    let original = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("system".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        older,
        ChatMessage {
            role: "assistant".into(),
            content: Some("older answer".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(101),
        },
        current.clone(),
    ];
    let anchor = RunMessageAnchor::from_messages(&original)
        .expect("current user message should be anchored");
    assert_eq!(anchor.resolve(&original).unwrap(), 3);

    let mut normalized_current = current.clone();
    normalized_current.images.as_mut().unwrap()[0].url.clear();
    let compressed = vec![
        original[0].clone(),
        ChatMessage {
            role: "assistant".into(),
            content: Some("compressed history summary".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(150),
        },
        normalized_current.clone(),
    ];
    assert_eq!(
        anchor
            .resolve(&compressed)
            .expect("compressed suffix should retain the anchor"),
        2
    );

    let mut missing = compressed.clone();
    missing.remove(2);
    assert!(anchor.resolve(&missing).is_err());

    let mut ambiguous = compressed;
    ambiguous.push(normalized_current);
    assert!(
        anchor.resolve(&ambiguous).is_err(),
        "an exact duplicate identity must fail closed instead of guessing"
    );
}

#[test]
fn stopped_terminal_patch_trims_only_the_run_tail_and_preserves_concurrent_fields() {
    let mut base = test_session("terminal-stopped-merge", "Stopped merge", None);
    base.messages.extend([
        ChatMessage {
            role: "user".into(),
            content: Some("stop after this tool starts".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(20),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("unfinished tool call".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(21),
        },
    ]);
    base.pending_plan = Some(completion_verifier_test_plan("terminal-stopped-plan"));
    let mut stopped = base.clone();
    stopped.messages.truncate(2);
    let stopped_plan = stopped.pending_plan.as_mut().expect("plan should exist");
    stopped_plan.status = crate::plan::PlanStatus::Stopped;
    stopped_plan.finished_at = Some(50);
    stopped.updated_at = 50;
    let anchor = RunMessageAnchor::from_messages(&base.messages)
        .expect("stopped user anchor should be captured");
    let patch = TerminalSessionPatch::new(&base, &stopped, &anchor)
        .expect("stopped patch should be constructible");

    let mut latest = base;
    latest.show_reasoning = false;
    latest.working_directory = temp_workspace("terminal-stopped-rebind");
    latest.todos.revision = 3;
    latest.updated_at = 60;
    let merged = patch
        .apply_to_latest(&latest)
        .expect("stopped patch should preserve non-run fields");
    assert!(terminal_patch_values_match(
        &merged.messages,
        &stopped.messages
    ));
    assert!(!merged.show_reasoning);
    assert_eq!(merged.working_directory, latest.working_directory);
    assert_eq!(merged.todos.revision, 3);
    assert_eq!(
        merged.pending_plan.as_ref().map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Stopped)
    );
    assert_eq!(merged.updated_at, 60);
}

#[tokio::test]
async fn finish_merges_a_todo_committed_after_candidate_preparation() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "finish-todo-merge-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("finish-todo-merge");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let plan = completion_verifier_test_plan("finish-todo-merge-plan");
    let mut session = test_session(&session_id, "Finish Todo merge", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.pending_plan = Some(plan.clone());
    session.messages.extend([
        ChatMessage {
            role: "user".into(),
            content: Some("complete the approved plan".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(70),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("approved result complete".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(71),
        },
    ]);
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let gate = Arc::new(FinishPhaseTestGate::new(
        FinishPhaseTestStage::PostSessionMutation,
    ));
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::Execute;
    phase_state.cycle_workspace = workspace;
    phase_state.approved_plan = Some(plan);
    phase_state.completion_evidence.finish_gate = Some(Arc::clone(&gate));
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let concurrent_todo = async {
        wait_for_finish_phase_gate(&gate).await;
        let _release = FinishPhaseGateRelease(Arc::clone(&gate));
        let response = crate::todos::replace_session_todos(
            &state,
            &session_id,
            crate::todos::TodoReplaceRequest {
                base_revision: 0,
                items: vec![crate::todos::TodoItem {
                    id: "concurrent".into(),
                    content: "Committed while Finish waits".into(),
                    status: crate::todos::TodoStatus::Pending,
                }],
            },
            crate::todos::TodoUpdateOrigin::User,
        )
        .await
        .expect("concurrent Todo update should persist");
        assert!(response.ok);
    };
    let (control, ()) = tokio::join!(run_finish_phase(&ctx, &mut phase_state), concurrent_todo);

    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(!phase_state.run_failed);
    let current = state.sessions.lock().await[&session_id].clone();
    assert_eq!(current.todos.revision, 1);
    assert_eq!(current.todos.items[0].id, "concurrent");
    assert_eq!(
        current.pending_plan.as_ref().map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Completed)
    );
    let persisted = crate::session_store::load_session_snapshot_from_path(
        &crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    )
    .expect("terminal snapshot should persist");
    assert_eq!(persisted.todos.revision, 1);
    assert_eq!(persisted.todos.items[0].id, "concurrent");
    assert_eq!(
        persisted.pending_plan.as_ref().map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Completed)
    );
}

#[tokio::test]
async fn terminal_merge_real_sqlite_preserves_concurrent_fields_and_run_boundaries() {
    let home = temp_workspace("terminal-merge-real-sqlite");
    std::fs::create_dir_all(&home).expect("database home should be created");
    let database = crate::storage::Database::open(home.join("lingclaw.db"))
        .await
        .expect("real SQLite database should open");

    let mut base = test_session("terminal-sqlite-complete", "Complete merge", None);
    base.version = crate::SESSION_VERSION;
    base.working_directory = home.clone();
    base.workspace = home.clone();
    base.messages.extend([
        ChatMessage {
            role: "user".into(),
            content: Some("complete transactionally".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(101),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("completed result".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(102),
        },
    ]);
    base.pending_plan = Some(completion_verifier_test_plan("terminal-sqlite-plan"));
    database
        .save_session(&base)
        .await
        .expect("base Session should persist");
    let expected = database
        .load_session(&base.id)
        .await
        .expect("base Session should load")
        .expect("base Session should exist");
    let mut terminal = expected.clone();
    let plan = terminal.pending_plan.as_mut().expect("plan should exist");
    plan.status = crate::plan::PlanStatus::Completed;
    plan.finished_at = Some(110);
    terminal.updated_at = 110;
    let anchor = RunMessageAnchor::from_messages(&expected.messages)
        .expect("SQLite run anchor should be captured");
    let patch = TerminalSessionPatch::new(&expected, &terminal, &anchor)
        .expect("completed patch should be constructed");

    let rebound = home.join("rebound-working-directory");
    std::fs::create_dir_all(&rebound).expect("rebound directory should exist");
    let mut independently_committed = expected.clone();
    independently_committed.name = "Concurrent rename".into();
    independently_committed.model_override = Some("provider/concurrent-model".into());
    independently_committed.think_level = "xhigh".into();
    independently_committed.working_directory = rebound.clone();
    independently_committed.workspace_kind = crate::SessionWorkspaceKind::Directory;
    independently_committed.input_tokens = 1234;
    independently_committed.output_tokens = 567;
    independently_committed.todos = crate::todos::TodoSnapshot {
        revision: 8,
        items: vec![crate::todos::TodoItem {
            id: "sqlite-todo".into(),
            content: "Preserve the independently committed Todo".into(),
            status: crate::todos::TodoStatus::Completed,
        }],
        last_updated_by: crate::todos::TodoUpdatedBy::User,
        updated_at: 120,
    };
    independently_committed.updated_at = 120;
    database
        .save_session(&independently_committed)
        .await
        .expect("concurrent writer should persist first");

    let latest = database
        .load_session(&base.id)
        .await
        .expect("latest Session should load")
        .expect("latest Session should exist");
    let merged = patch
        .apply_to_latest(&latest)
        .expect("terminal fields should merge into the latest Session");
    database
        .save_session_with_run_outcome(
            &merged,
            terminal_merge_test_outcome(
                &merged,
                "run-terminal-sqlite-complete",
                "completed",
                "finish",
            ),
        )
        .await
        .expect("terminal Session and outcome should commit atomically");

    let stored = database
        .load_session(&base.id)
        .await
        .expect("committed Session should load")
        .expect("committed Session should exist");
    assert_eq!(stored.name, "Concurrent rename");
    assert_eq!(
        stored.model_override.as_deref(),
        Some("provider/concurrent-model")
    );
    assert_eq!(stored.think_level, "xhigh");
    assert_eq!(stored.working_directory, rebound);
    assert_eq!(
        stored.workspace_kind,
        crate::SessionWorkspaceKind::Directory
    );
    assert_eq!(stored.input_tokens, 1234);
    assert_eq!(stored.output_tokens, 567);
    assert_eq!(stored.todos.revision, 8);
    assert_eq!(
        stored.pending_plan.as_ref().map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Completed)
    );
    let outcomes = database
        .load_run_outcomes(&base.id)
        .await
        .expect("terminal outcome should load");
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, "completed");

    let mut stopped_base = expected.clone();
    stopped_base.id = "terminal-sqlite-stopped".into();
    stopped_base.name = "Stopped merge".into();
    stopped_base.pending_plan = Some(completion_verifier_test_plan(
        "terminal-sqlite-stopped-plan",
    ));
    database
        .save_session(&stopped_base)
        .await
        .expect("stopped base should persist");
    let stopped_expected = database
        .load_session(&stopped_base.id)
        .await
        .expect("stopped base should load")
        .expect("stopped base should exist");
    let mut stopped_terminal = stopped_expected.clone();
    stopped_terminal.messages.pop();
    let plan = stopped_terminal
        .pending_plan
        .as_mut()
        .expect("stopped plan should exist");
    plan.status = crate::plan::PlanStatus::Stopped;
    plan.finished_at = Some(130);
    let stopped_anchor = RunMessageAnchor::from_messages(&stopped_expected.messages)
        .expect("stopped SQLite anchor should be captured");
    let stopped_patch =
        TerminalSessionPatch::new(&stopped_expected, &stopped_terminal, &stopped_anchor)
            .expect("stopped patch should be constructed");
    let mut stopped_concurrent = stopped_expected.clone();
    stopped_concurrent.todos.revision = 9;
    stopped_concurrent.model_override = Some("provider/stopped-concurrent".into());
    database
        .save_session(&stopped_concurrent)
        .await
        .expect("stopped concurrent writer should persist");
    let stopped_latest = database
        .load_session(&stopped_base.id)
        .await
        .expect("stopped latest Session should load")
        .expect("stopped latest Session should exist");
    let stopped_merged = stopped_patch
        .apply_to_latest(&stopped_latest)
        .expect("stopped fields should merge");
    database
        .save_session_with_run_outcome(
            &stopped_merged,
            terminal_merge_test_outcome(
                &stopped_merged,
                "run-terminal-sqlite-stopped",
                "stopped",
                "stopped",
            ),
        )
        .await
        .expect("stopped outcome should commit");
    let stored_stopped = database
        .load_session(&stopped_base.id)
        .await
        .expect("stopped Session should load")
        .expect("stopped Session should exist");
    assert_eq!(
        stored_stopped.messages.len(),
        stopped_terminal.messages.len()
    );
    assert_eq!(stored_stopped.todos.revision, 9);
    assert_eq!(
        stored_stopped.model_override.as_deref(),
        Some("provider/stopped-concurrent")
    );
    assert_eq!(
        stored_stopped.pending_plan.as_ref().map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Stopped)
    );

    let mut boundary_base = expected;
    boundary_base.id = "terminal-sqlite-boundary".into();
    boundary_base.pending_plan = Some(completion_verifier_test_plan(
        "terminal-sqlite-boundary-plan",
    ));
    database
        .save_session(&boundary_base)
        .await
        .expect("boundary base should persist");
    let boundary_expected = database
        .load_session(&boundary_base.id)
        .await
        .expect("boundary base should load")
        .expect("boundary base should exist");
    let mut boundary_terminal = boundary_expected.clone();
    boundary_terminal
        .pending_plan
        .as_mut()
        .expect("boundary plan should exist")
        .status = crate::plan::PlanStatus::Completed;
    let boundary_anchor = RunMessageAnchor::from_messages(&boundary_expected.messages)
        .expect("boundary anchor should be captured");
    let boundary_patch =
        TerminalSessionPatch::new(&boundary_expected, &boundary_terminal, &boundary_anchor)
            .expect("boundary patch should be constructed");
    let mut next_run = boundary_expected.clone();
    next_run.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("deferred next-run message".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(140),
    });
    database
        .save_session(&next_run)
        .await
        .expect("next-run message should persist first");
    let next_latest = database
        .load_session(&boundary_base.id)
        .await
        .expect("next-run Session should load")
        .expect("next-run Session should exist");
    assert!(
        boundary_patch.apply_to_latest(&next_latest).is_err(),
        "an old terminal patch must not adopt or erase a next-run message"
    );
    assert!(
        database
            .load_run_outcomes(&boundary_base.id)
            .await
            .expect("boundary outcomes should load")
            .is_empty()
    );

    database
        .close_for_test()
        .await
        .expect("real SQLite connection should close");
    std::fs::remove_dir_all(&home).expect("real SQLite test home should be removable");
}

#[tokio::test]
async fn terminal_sqlite_response_delay_keeps_later_writers_behind_the_persist_gate() {
    let home = temp_workspace("terminal-response-gate-real-sqlite");
    std::fs::create_dir_all(&home).expect("database home should be created");
    let database = crate::storage::Database::open(home.join("lingclaw.db"))
        .await
        .expect("real SQLite database should open");
    let mut session = test_session("terminal-response-gate", "Response gate", None);
    session.version = crate::SESSION_VERSION;
    session.working_directory = home.clone();
    session.workspace = home.clone();
    session.messages.extend([
        ChatMessage {
            role: "user".into(),
            content: Some("commit before releasing the gate".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(201),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("committed result".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(202),
        },
    ]);
    session.pending_plan = Some(completion_verifier_test_plan("terminal-response-gate-plan"));
    database
        .save_session(&session)
        .await
        .expect("base Session should persist");
    let mut terminal = database
        .load_session(&session.id)
        .await
        .expect("base Session should load")
        .expect("base Session should exist");
    terminal
        .pending_plan
        .as_mut()
        .expect("terminal plan should exist")
        .status = crate::plan::PlanStatus::Completed;

    let database_gate = Arc::new(crate::storage::RunOutcomeCommitTestGate::default());
    let _database_gate_release = RuntimeOutcomeGateRelease(Arc::clone(&database_gate));
    database_gate.allow_commit();
    let persist_gate = crate::session_store::session_persist_gate(&session.id);
    let terminal_task = {
        let database = database.clone();
        let terminal = terminal.clone();
        let database_gate = Arc::clone(&database_gate);
        let persist_gate = Arc::clone(&persist_gate);
        tokio::spawn(async move {
            let _guard = persist_gate.lock().await;
            database
                .save_session_with_run_outcome_test_gated(
                    &terminal,
                    terminal_merge_test_outcome(
                        &terminal,
                        "run-terminal-response-gate",
                        "completed",
                        "finish",
                    ),
                    database_gate,
                )
                .await
        })
    };
    wait_for_runtime_outcome_gate(|| database_gate.committed()).await;

    let writer_attempted = Arc::new(tokio::sync::Notify::new());
    let writer_acquired = Arc::new(tokio::sync::Notify::new());
    let writer_task = {
        let database = database.clone();
        let session_id = session.id.clone();
        let persist_gate = Arc::clone(&persist_gate);
        let writer_attempted = Arc::clone(&writer_attempted);
        let writer_acquired = Arc::clone(&writer_acquired);
        tokio::spawn(async move {
            writer_attempted.notify_one();
            let _guard = persist_gate.lock().await;
            writer_acquired.notify_one();
            let mut latest = database
                .load_session(&session_id)
                .await?
                .ok_or_else(|| crate::storage::StorageError::new("missing Session"))?;
            latest.model_override = Some("provider/after-terminal".into());
            latest.think_level = "high".into();
            latest.todos = crate::todos::TodoSnapshot {
                revision: 2,
                items: vec![crate::todos::TodoItem {
                    id: "after-terminal".into(),
                    content: "Writer waited for terminal acknowledgement".into(),
                    status: crate::todos::TodoStatus::Pending,
                }],
                last_updated_by: crate::todos::TodoUpdatedBy::User,
                updated_at: 220,
            };
            database.save_session(&latest).await
        })
    };
    writer_attempted.notified().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(75), writer_acquired.notified())
            .await
            .is_err(),
        "a later writer must remain blocked until the terminal database response returns"
    );

    database_gate.allow_response();
    terminal_task
        .await
        .expect("terminal task should join")
        .expect("terminal transaction should succeed");
    writer_task
        .await
        .expect("writer task should join")
        .expect("writer should save after the gate releases");

    let stored = database
        .load_session(&session.id)
        .await
        .expect("final Session should load")
        .expect("final Session should exist");
    assert_eq!(
        stored.pending_plan.as_ref().map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Completed)
    );
    assert_eq!(
        stored.model_override.as_deref(),
        Some("provider/after-terminal")
    );
    assert_eq!(stored.think_level, "high");
    assert_eq!(stored.todos.revision, 2);
    assert_eq!(
        database
            .load_run_outcomes(&session.id)
            .await
            .expect("outcome should load")
            .len(),
        1
    );

    database
        .close_for_test()
        .await
        .expect("response-gate SQLite connection should close");
    std::fs::remove_dir_all(&home).expect("response-gate test home should be removable");
}

#[tokio::test]
async fn auxiliary_usage_waits_for_terminal_ack_is_idempotent_and_survives_reopen() {
    let home = temp_workspace("auxiliary-usage-terminal-gate");
    std::fs::create_dir_all(&home).expect("database home should be created");
    let database_path = home.join("lingclaw.db");
    let database = crate::storage::Database::open(database_path.clone())
        .await
        .expect("real SQLite database should open");
    let mut session = test_session("auxiliary-usage-terminal-gate", "Aux Usage", None);
    session.version = crate::SESSION_VERSION;
    session.workspace = home.clone();
    session.working_directory = home.clone();
    session.messages.extend([
        ChatMessage {
            role: "user".into(),
            content: Some("finish before auxiliary usage".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(301),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("terminal result".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(302),
        },
    ]);
    database
        .save_session(&session)
        .await
        .expect("base Session should persist");
    let sessions = Arc::new(Mutex::new(HashMap::from([(
        session.id.clone(),
        session.clone(),
    )])));

    let database_gate = Arc::new(crate::storage::RunOutcomeCommitTestGate::default());
    let _database_gate_release = RuntimeOutcomeGateRelease(Arc::clone(&database_gate));
    database_gate.allow_commit();
    let persist_gate = crate::session_store::session_persist_gate(&session.id);
    let terminal_task = {
        let database = database.clone();
        let session = session.clone();
        let database_gate = Arc::clone(&database_gate);
        let persist_gate = Arc::clone(&persist_gate);
        tokio::spawn(async move {
            let _guard = persist_gate.lock().await;
            database
                .save_session_with_run_outcome_test_gated(
                    &session,
                    terminal_merge_test_outcome(
                        &session,
                        "run-auxiliary-usage-terminal-gate",
                        "completed",
                        "finish",
                    ),
                    database_gate,
                )
                .await
        })
    };
    wait_for_runtime_outcome_gate(|| database_gate.committed()).await;

    let reflection_update = crate::context::UsageUpdate {
        input_tokens: 11,
        output_tokens: 7,
        input_source: "provider".into(),
        output_source: "provider".into(),
        labels: crate::context::build_usage_labels(
            11,
            7,
            Some("mock-provider"),
            Some(crate::context::USAGE_ROLE_REFLECTION),
        ),
    };
    let memory_update = crate::context::UsageUpdate {
        input_tokens: 5,
        output_tokens: 3,
        input_source: "provider".into(),
        output_source: "provider".into(),
        labels: crate::context::build_usage_labels(
            5,
            3,
            Some("mock-provider"),
            Some(crate::context::USAGE_ROLE_MEMORY),
        ),
    };
    let reflection_attempted = Arc::new(tokio::sync::Notify::new());
    let memory_attempted = Arc::new(tokio::sync::Notify::new());
    let reflection_task = {
        let database = database.clone();
        let sessions = Arc::clone(&sessions);
        let session_id = session.id.clone();
        let update = reflection_update.clone();
        let attempted = Arc::clone(&reflection_attempted);
        tokio::spawn(async move {
            attempted.notify_one();
            crate::session_store::persist_auxiliary_usage_update_with_database(
                database,
                sessions,
                session_id,
                "reflection-provider-call-1".into(),
                update,
            )
            .await
        })
    };
    let memory_task = {
        let database = database.clone();
        let sessions = Arc::clone(&sessions);
        let session_id = session.id.clone();
        let update = memory_update.clone();
        let attempted = Arc::clone(&memory_attempted);
        tokio::spawn(async move {
            attempted.notify_one();
            crate::session_store::persist_auxiliary_usage_update_with_database(
                database,
                sessions,
                session_id,
                "memory-provider-call-1".into(),
                update,
            )
            .await
        })
    };
    reflection_attempted.notified().await;
    memory_attempted.notified().await;
    tokio::task::yield_now().await;
    assert!(!reflection_task.is_finished());
    assert!(!memory_task.is_finished());
    assert_eq!(sessions.lock().await[&session.id].input_tokens, 0);

    database_gate.allow_response();
    terminal_task
        .await
        .expect("terminal task should join")
        .expect("terminal transaction should succeed");
    assert_eq!(
        reflection_task
            .await
            .expect("reflection Usage task should join")
            .expect("reflection Usage should persist"),
        crate::storage::AuxiliaryUsageApplyOutcome::Applied,
    );
    assert_eq!(
        memory_task
            .await
            .expect("memory Usage task should join")
            .expect("memory Usage should persist"),
        crate::storage::AuxiliaryUsageApplyOutcome::Applied,
    );

    let in_memory = sessions.lock().await[&session.id].clone();
    assert_eq!(in_memory.input_tokens, 16);
    assert_eq!(in_memory.output_tokens, 10);
    assert_eq!(
        in_memory
            .total_label_usage
            .get(&crate::context::usage_role_label(
                crate::context::USAGE_ROLE_REFLECTION
            )),
        Some(&[11, 7])
    );
    assert_eq!(
        in_memory
            .total_label_usage
            .get(&crate::context::usage_role_label(
                crate::context::USAGE_ROLE_MEMORY
            )),
        Some(&[5, 3])
    );

    assert_eq!(
        crate::session_store::persist_auxiliary_usage_update_with_database(
            database.clone(),
            Arc::clone(&sessions),
            session.id.clone(),
            "reflection-provider-call-1".into(),
            reflection_update,
        )
        .await
        .expect("a duplicate operation should be recognized"),
        crate::storage::AuxiliaryUsageApplyOutcome::Duplicate,
    );
    assert_eq!(sessions.lock().await[&session.id].input_tokens, 16);

    database
        .close_for_test()
        .await
        .expect("SQLite connection should close immediately after Usage success");
    let reopened = crate::storage::Database::open(database_path)
        .await
        .expect("database should reopen after immediate shutdown");
    let usage = reopened
        .load_usage_snapshot(&session.id, &prompts::current_local_snapshot().today())
        .await
        .expect("Usage snapshot should load")
        .expect("Usage snapshot should exist");
    assert_eq!(usage.total_input, 16);
    assert_eq!(usage.total_output, 10);
    assert_eq!(
        usage.total_labels.get(&crate::context::usage_role_label(
            crate::context::USAGE_ROLE_REFLECTION
        )),
        Some(&[11, 7])
    );
    assert_eq!(
        usage.total_labels.get(&crate::context::usage_role_label(
            crate::context::USAGE_ROLE_MEMORY
        )),
        Some(&[5, 3])
    );
    reopened
        .close_for_test()
        .await
        .expect("reopened SQLite connection should close");
    std::fs::remove_dir_all(&home).expect("auxiliary Usage test home should be removable");
}

#[tokio::test]
async fn auxiliary_usage_failure_protects_storage_without_changing_memory() {
    let home = temp_workspace("auxiliary-usage-failure");
    std::fs::create_dir_all(&home).expect("database home should be created");
    let database = crate::storage::Database::open(home.join("lingclaw.db"))
        .await
        .expect("real SQLite database should open");
    let mut session = test_session("auxiliary-usage-failure", "Aux failure", None);
    session.version = crate::SESSION_VERSION;
    session.workspace = home.clone();
    session.working_directory = home.clone();
    database
        .save_session(&session)
        .await
        .expect("base Session should persist");
    let sessions = Arc::new(Mutex::new(HashMap::from([(
        session.id.clone(),
        session.clone(),
    )])));
    let result = crate::session_store::persist_auxiliary_usage_update_with_database(
        database.clone(),
        Arc::clone(&sessions),
        session.id.clone(),
        "overflowing-auxiliary-operation".into(),
        crate::context::UsageUpdate {
            input_tokens: u64::MAX,
            output_tokens: 1,
            input_source: "provider".into(),
            output_source: "provider".into(),
            labels: HashMap::new(),
        },
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        database.status().mode,
        crate::storage::StorageMode::Protected
    );
    let in_memory = sessions.lock().await[&session.id].clone();
    assert_eq!(in_memory.input_tokens, 0);
    assert_eq!(in_memory.output_tokens, 0);

    database
        .close_for_test()
        .await
        .expect("failed Usage database should close");
    std::fs::remove_dir_all(&home).expect("failed Usage test home should be removable");
}

#[tokio::test]
async fn auxiliary_usage_missing_is_a_domain_outcome_and_unloaded_sessions_persist() {
    let home = temp_workspace("auxiliary-usage-domain-outcomes");
    std::fs::create_dir_all(&home).expect("database home should be created");
    let database = crate::storage::Database::open(home.join("lingclaw.db"))
        .await
        .expect("real SQLite database should open");
    let mut session = test_session("auxiliary-usage-unloaded", "Unloaded Usage", None);
    session.version = crate::SESSION_VERSION;
    session.workspace = home.clone();
    session.working_directory = home.clone();
    database
        .save_session(&session)
        .await
        .expect("base Session should persist");
    let sessions = Arc::new(Mutex::new(HashMap::new()));
    let update = crate::context::UsageUpdate {
        input_tokens: 9,
        output_tokens: 4,
        input_source: "provider".into(),
        output_source: "provider".into(),
        labels: crate::context::build_usage_labels(
            9,
            4,
            Some("mock-provider"),
            Some(crate::context::USAGE_ROLE_MEMORY),
        ),
    };

    assert_eq!(
        crate::session_store::persist_auxiliary_usage_update_with_database(
            database.clone(),
            Arc::clone(&sessions),
            session.id.clone(),
            "unloaded-session-operation".into(),
            update.clone(),
        )
        .await
        .expect("unloaded Session Usage should persist"),
        crate::storage::AuxiliaryUsageApplyOutcome::Applied,
    );
    assert!(sessions.lock().await.is_empty());
    let persisted = database
        .load_usage_snapshot(&session.id, &prompts::current_local_snapshot().today())
        .await
        .expect("Usage snapshot should load")
        .expect("unloaded Session Usage should exist");
    assert_eq!((persisted.total_input, persisted.total_output), (9, 4));

    assert_eq!(
        crate::session_store::persist_auxiliary_usage_update_with_database(
            database.clone(),
            Arc::clone(&sessions),
            "deleted-session".into(),
            "late-deleted-session-operation".into(),
            update,
        )
        .await
        .expect("missing Session is a normal domain result"),
        crate::storage::AuxiliaryUsageApplyOutcome::Missing,
    );
    assert_eq!(database.status().mode, crate::storage::StorageMode::Healthy);
    assert!(
        database
            .load_usage_snapshot(
                "deleted-session",
                &prompts::current_local_snapshot().today()
            )
            .await
            .expect("missing Usage lookup should succeed")
            .is_none()
    );

    database
        .close_for_test()
        .await
        .expect("domain outcome database should close");
    std::fs::remove_dir_all(&home).expect("domain outcome test home should be removable");
}

#[tokio::test]
async fn successful_memory_provider_race_is_drained_before_session_workspace_deletion() {
    let (api_base, provider, _provider_guard) = spawn_blocking_auxiliary_provider(
        r#"{"update_facts":[{"key":"drained","value":"yes"}],"delete_facts":[]}"#,
    )
    .await;
    let mut config = test_config();
    config.api_base = api_base;
    config.model = "memory-barrier-model".to_string();
    config.structured_memory = true;
    config.max_llm_retries = 0;
    let state = Arc::new(test_app_state_with_config(config.clone()));
    let session_id = format!(
        "memory-delete-race-{}",
        crate::generate_random_session_id().expect("random Session id")
    );
    let _artifacts = ManagedSessionArtifactsGuard::new(&session_id);
    let workspace = crate::session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("managed workspace should exist");
    let mut session = test_session(&session_id, "Memory delete race", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.messages.extend([
        ChatMessage {
            role: "user".into(),
            content: Some("Remember the deletion barrier".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(1),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("I will remember it".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(2),
        },
    ]);
    crate::session_store::save_session_to_disk(&session)
        .await
        .expect("Session should persist");
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session.clone());
    let queue = crate::memory::MemoryUpdateQueue::spawn(
        config.clone(),
        Arc::clone(&state.sessions),
        state.auxiliary_tasks.clone(),
    );
    {
        let mut memory_queue = state.memory_queue.lock().expect("memory queue lock");
        *memory_queue = Some(queue.clone());
    }

    // Keep the Usage stage behind the exact Session gate. The Provider-stage
    // barrier proves the real response succeeded before deletion closes the
    // allocation; Usage must still commit, while the later private write must
    // be rejected by the closed lifecycle.
    let persist_gate = crate::session_store::session_persist_gate(&session_id);
    let persist_guard = persist_gate.lock().await;
    let provider_succeeded = Arc::new(tokio::sync::Notify::new());
    let release_provider_stage = Arc::new(tokio::sync::Notify::new());
    queue.enqueue_with_test_controls(
        session_id.clone(),
        workspace.clone(),
        config.model.clone(),
        Arc::new(config),
        crate::memory::prefilter_for_memory(&session.messages),
        crate::memory::MemoryUpdateTestControls {
            usage_database: None,
            post_provider_gate: Some((
                Arc::clone(&provider_succeeded),
                Arc::clone(&release_provider_stage),
            )),
            post_usage_gate: None,
            force_private_save_failure: false,
        },
    );
    tokio::time::timeout(Duration::from_secs(6), provider.arrived.notified())
        .await
        .expect("Memory Provider request should arrive");
    provider.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), provider_succeeded.notified())
        .await
        .expect("successful Provider response should reach the Usage boundary");

    let delete_state = Arc::clone(&state);
    let delete_session_id = session_id.clone();
    let mut deletion = tokio::spawn(async move {
        crate::session_control::delete_session_with_safety_checks(
            &delete_state,
            &delete_session_id,
            Some(MAIN_SESSION_ID),
        )
        .await
    });
    let lifecycle_closed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state
                .auxiliary_tasks
                .permit(
                    &session_id,
                    crate::auxiliary_tasks::AuxiliaryTaskKind::Memory,
                )
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let deletion_waited_for_usage = !deletion.is_finished();

    release_provider_stage.notify_one();
    drop(persist_guard);
    let delete_result = (&mut deletion)
        .await
        .expect("deletion task should join")
        .expect("Session deletion should succeed after Usage drains");
    queue.shutdown_and_wait().await;

    assert!(lifecycle_closed, "deletion must close new registrations");
    assert!(
        deletion_waited_for_usage,
        "deletion must await the post-Provider Usage stage"
    );
    assert!(delete_result.contains("Deleted"));
    assert_eq!(provider.requests.lock().await.len(), 1);
    assert!(
        !workspace.join("structured_memory.json").exists(),
        "a lifecycle closed before the private-write boundary must suppress the write"
    );
    assert!(
        !workspace.exists(),
        "the drained task must not recreate a deleted workspace"
    );
    assert_eq!(state.auxiliary_tasks.task_count(), 0);
}

#[tokio::test]
async fn memory_provider_usage_survives_feature_disable_before_private_write() {
    let (api_base, provider, _provider_guard) = spawn_blocking_auxiliary_provider(
        r#"{"update_facts":[{"key":"disabled","value":"no late write"}],"delete_facts":[]}"#,
    )
    .await;
    let mut config = test_config();
    config.api_base = api_base;
    config.model = "memory-disable-model".to_string();
    config.structured_memory = true;
    config.max_llm_retries = 0;
    let state = Arc::new(test_app_state_with_config(config.clone()));
    let session_id = format!(
        "memory-disable-race-{}",
        crate::generate_random_session_id().expect("random Session id")
    );
    let workspace = temp_workspace("memory-disable-race");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should exist");
    let mut session = test_session(&session_id, "Memory disable race", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Remember the feature-disable boundary".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(1),
    });
    crate::session_store::save_session_to_disk(&session)
        .await
        .expect("Session should persist");
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session.clone());
    let queue = crate::memory::MemoryUpdateQueue::spawn(
        config.clone(),
        Arc::clone(&state.sessions),
        state.auxiliary_tasks.clone(),
    );
    *state.memory_queue.lock().expect("Memory queue lock") = Some(queue.clone());
    let provider_succeeded = Arc::new(tokio::sync::Notify::new());
    let release_provider_stage = Arc::new(tokio::sync::Notify::new());
    queue.enqueue_with_test_controls(
        session_id.clone(),
        workspace.clone(),
        config.model.clone(),
        Arc::new(config.clone()),
        crate::memory::prefilter_for_memory(&session.messages),
        crate::memory::MemoryUpdateTestControls {
            usage_database: None,
            post_provider_gate: Some((
                Arc::clone(&provider_succeeded),
                Arc::clone(&release_provider_stage),
            )),
            post_usage_gate: None,
            force_private_save_failure: false,
        },
    );
    tokio::time::timeout(Duration::from_secs(6), provider.arrived.notified())
        .await
        .expect("Memory Provider request should arrive");
    provider.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), provider_succeeded.notified())
        .await
        .expect("Provider should succeed before feature disable");

    let disable_state = Arc::clone(&state);
    let mut disabled = config;
    disabled.structured_memory = false;
    let mut disable = tokio::spawn(async move {
        disable_state.sync_memory_queue(&disabled).await;
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state
                .auxiliary_tasks
                .permit(
                    &session_id,
                    crate::auxiliary_tasks::AuxiliaryTaskKind::Memory,
                )
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("feature disable should invalidate the Memory cycle");
    assert!(
        !disable.is_finished(),
        "feature disable must drain the task"
    );
    release_provider_stage.notify_one();
    (&mut disable)
        .await
        .expect("feature disable should finish cleanly");

    let in_memory = state.sessions.lock().await[&session_id].clone();
    let persisted = crate::session_store::load_session_from_disk(&session_id)
        .expect("persisted Session should remain");
    assert_eq!((in_memory.input_tokens, in_memory.output_tokens), (17, 9));
    assert_eq!((persisted.input_tokens, persisted.output_tokens), (17, 9));
    assert!(!workspace.join("structured_memory.json").exists());
    assert!(!workspace.join("structured_memory.audit.jsonl").exists());
    assert_eq!(state.auxiliary_tasks.task_count(), 0);
}

#[tokio::test]
async fn successful_reflection_provider_race_is_drained_before_session_workspace_deletion() {
    let _reflection_guard = reflection_test_guard().lock().await;
    let _runtime_reset = ReflectionRuntimeReset;
    refresh_reflection_runtime(true);
    let reflection_generation = reflection_runtime_generation();
    let (api_base, provider, _provider_guard) =
        spawn_blocking_auxiliary_provider("- Keep lifecycle ownership explicit").await;
    let mut config = test_config();
    config.api_base = api_base;
    config.model = "reflection-barrier-model".to_string();
    config.daily_reflection = true;
    config.max_llm_retries = 0;
    let state = Arc::new(test_app_state_with_config(config.clone()));
    let session_id = format!(
        "reflection-delete-race-{}",
        crate::generate_random_session_id().expect("random Session id")
    );
    let _artifacts = ManagedSessionArtifactsGuard::new(&session_id);
    let workspace = crate::session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("managed workspace should exist");
    let mut session = test_session(&session_id, "Reflection delete race", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.messages.extend([
        ChatMessage {
            role: "user".into(),
            content: Some("Reflect on this completed task".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(1),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("The task completed".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(2),
        },
    ]);
    crate::session_store::save_session_to_disk(&session)
        .await
        .expect("Session should persist");
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session.clone());

    let reached_post_provider = Arc::new(tokio::sync::Notify::new());
    let release_post_provider = Arc::new(tokio::sync::Notify::new());
    let permit = state
        .auxiliary_tasks
        .permit(
            &session_id,
            crate::auxiliary_tasks::AuxiliaryTaskKind::Reflection,
        )
        .expect("Reflection permit should be available");
    let reflection = state
        .auxiliary_tasks
        .spawn(permit, {
            let state = Arc::clone(&state);
            let config = Arc::new(config);
            let session_id = session_id.clone();
            let workspace = workspace.clone();
            let messages = session.messages.clone();
            let provider_gate = (
                Arc::clone(&reached_post_provider),
                Arc::clone(&release_post_provider),
            );
            move |task_context| async move {
                run_post_execution_reflection(PostExecutionReflectionInput {
                    config,
                    http: state.http.clone(),
                    sessions: Arc::clone(&state.sessions),
                    session_id,
                    workspace,
                    model: "reflection-barrier-model".to_string(),
                    messages,
                    policy_generation: reflection_generation,
                    cycles: 4,
                    tool_calls: 2,
                    provider_timeout: Duration::from_secs(30),
                    task_context,
                    provider_succeeded_gate: Some(provider_gate),
                })
                .await
            }
        })
        .expect("Reflection task should be supervised");
    tokio::time::timeout(Duration::from_secs(3), provider.arrived.notified())
        .await
        .expect("Reflection Provider request should arrive");
    provider.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), reached_post_provider.notified())
        .await
        .expect("Reflection Provider should succeed before deletion");

    let delete_state = Arc::clone(&state);
    let delete_session_id = session_id.clone();
    let mut deletion = tokio::spawn(async move {
        crate::session_control::delete_session_with_safety_checks(
            &delete_state,
            &delete_session_id,
            Some(MAIN_SESSION_ID),
        )
        .await
    });
    let lifecycle_closed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state
                .auxiliary_tasks
                .permit(
                    &session_id,
                    crate::auxiliary_tasks::AuxiliaryTaskKind::Reflection,
                )
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let deletion_waited_for_reflection = !deletion.is_finished();

    release_post_provider.notify_one();
    let reflection_written = reflection
        .wait()
        .await
        .expect("Reflection supervisor should finish")
        .expect("Reflection task should not fail");
    let delete_result = (&mut deletion)
        .await
        .expect("deletion task should join")
        .expect("Session deletion should succeed after Reflection drains");

    assert!(lifecycle_closed, "deletion must close new registrations");
    assert!(
        deletion_waited_for_reflection,
        "deletion must await the post-Provider Reflection stage"
    );
    assert!(
        !reflection_written,
        "a cancelled Reflection must not write after the Session closes"
    );
    assert!(delete_result.contains("Deleted"));
    assert_eq!(provider.requests.lock().await.len(), 1);
    assert!(!workspace.exists());
    assert_eq!(state.auxiliary_tasks.task_count(), 0);
}

#[tokio::test]
async fn graceful_auxiliary_shutdown_drains_post_provider_usage_before_returning() {
    let _reflection_guard = reflection_test_guard().lock().await;
    let _runtime_reset = ReflectionRuntimeReset;
    refresh_reflection_runtime(true);
    let reflection_generation = reflection_runtime_generation();
    let (api_base, provider, _provider_guard) =
        spawn_blocking_auxiliary_provider("- Persist Usage before shutdown returns").await;
    let mut config = test_config();
    config.api_base = api_base;
    config.model = "reflection-shutdown-model".to_string();
    config.daily_reflection = true;
    config.max_llm_retries = 0;
    let state = Arc::new(test_app_state_with_config(config.clone()));
    let session_id = format!(
        "reflection-shutdown-{}",
        crate::generate_random_session_id().expect("random Session id")
    );
    let workspace = temp_workspace("reflection-shutdown");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should exist");
    let mut session = test_session(&session_id, "Reflection shutdown", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.messages.extend([
        ChatMessage {
            role: "user".into(),
            content: Some("Finish this task and reflect".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(1),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("Task finished".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(2),
        },
    ]);
    crate::session_store::save_session_to_disk(&session)
        .await
        .expect("Session should persist");
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session.clone());

    let reached_post_provider = Arc::new(tokio::sync::Notify::new());
    let release_post_provider = Arc::new(tokio::sync::Notify::new());
    let permit = state
        .auxiliary_tasks
        .permit(
            &session_id,
            crate::auxiliary_tasks::AuxiliaryTaskKind::Reflection,
        )
        .expect("Reflection permit should be available");
    let reflection = state
        .auxiliary_tasks
        .spawn(permit, {
            let state = Arc::clone(&state);
            let config = Arc::new(config);
            let session_id = session_id.clone();
            let workspace = workspace.clone();
            let messages = session.messages.clone();
            let provider_gate = (
                Arc::clone(&reached_post_provider),
                Arc::clone(&release_post_provider),
            );
            move |task_context| async move {
                run_post_execution_reflection(PostExecutionReflectionInput {
                    config,
                    http: state.http.clone(),
                    sessions: Arc::clone(&state.sessions),
                    session_id,
                    workspace,
                    model: "reflection-shutdown-model".to_string(),
                    messages,
                    policy_generation: reflection_generation,
                    cycles: 4,
                    tool_calls: 2,
                    provider_timeout: Duration::from_secs(30),
                    task_context,
                    provider_succeeded_gate: Some(provider_gate),
                })
                .await
            }
        })
        .expect("Reflection task should be supervised");
    tokio::time::timeout(Duration::from_secs(3), provider.arrived.notified())
        .await
        .expect("Reflection Provider request should arrive");
    provider.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), reached_post_provider.notified())
        .await
        .expect("Reflection Provider should succeed before shutdown");

    let shutdown_state = Arc::clone(&state);
    let mut shutdown = tokio::spawn(async move {
        shutdown_state.shutdown_auxiliary_tasks().await;
    });
    let accepting_closed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if state
                .auxiliary_tasks
                .permit(
                    &session_id,
                    crate::auxiliary_tasks::AuxiliaryTaskKind::Reflection,
                )
                .is_err()
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .is_ok();
    let shutdown_waited = !shutdown.is_finished();

    release_post_provider.notify_one();
    let reflection_written = reflection
        .wait()
        .await
        .expect("Reflection supervisor should finish")
        .expect("Reflection task should not fail");
    (&mut shutdown)
        .await
        .expect("graceful auxiliary shutdown should join");

    let in_memory = state.sessions.lock().await[&session_id].clone();
    let persisted = crate::session_store::load_session_from_disk(&session_id)
        .expect("persisted Session should exist");
    assert!(accepting_closed);
    assert!(shutdown_waited, "shutdown must drain the registered task");
    assert!(
        !reflection_written,
        "shutdown must suppress the late file write"
    );
    assert_eq!((in_memory.input_tokens, in_memory.output_tokens), (17, 9));
    assert_eq!((persisted.input_tokens, persisted.output_tokens), (17, 9));
    assert!(!workspace.join("memory").exists());
    assert_eq!(state.auxiliary_tasks.task_count(), 0);
}

#[tokio::test]
async fn run_finish_phase_shutdown_cancels_a_slow_completion_verifier_without_failure_event() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "finish-contract-shutdown-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("finish-contract-shutdown");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let plan = completion_verifier_test_plan("plan_finish_contract_shutdown");
    let mut session = test_session(&session_id, "Finish contract shutdown", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.pending_plan = Some(plan.clone());
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let cancel = CancellationToken::new();
    let run_cancel = cancel.child_token();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let gate = Arc::new(CompletionVerifierTestGate::default());
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::Execute;
    phase_state.cycle_workspace = workspace.clone();
    phase_state.approved_plan = Some(plan);
    phase_state.completion_evidence.verifier_gate = Some(Arc::clone(&gate));
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let trigger = async {
        wait_for_completion_verifier_gate(&gate).await;
        cancel.cancel();
    };
    let (control, ()) = tokio::join!(run_finish_phase(&ctx, &mut phase_state), trigger);

    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(phase_state.shutting_down);
    assert!(!phase_state.run_failed);
    wait_for_completion_verifier_stop(&gate).await;
    let persisted_plan = state.sessions.lock().await[&session_id]
        .pending_plan
        .clone()
        .expect("executing plan should remain untouched for shutdown handling");
    assert_eq!(persisted_plan.status, crate::plan::PlanStatus::Executing);
    assert!(persisted_plan.finished_at.is_none());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(live_rx.try_recv().is_err());
}

#[tokio::test]
async fn run_finish_phase_completion_deadline_fails_closed_without_late_events() {
    let mut config = test_config();
    config.tool_timeout = Duration::from_millis(25);
    let state = Arc::new(test_app_state_with_config(config));
    let session_id = format!(
        "finish-contract-timeout-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("finish-contract-timeout");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let plan = completion_verifier_test_plan("plan_finish_contract_timeout");
    let mut session = test_session(&session_id, "Finish contract timeout", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("verify and finish this plan".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.pending_plan = Some(plan.clone());
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let gate = Arc::new(CompletionVerifierTestGate::default());
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::Execute;
    phase_state.cycle_workspace = workspace.clone();
    phase_state.approved_plan = Some(plan);
    phase_state.completion_evidence.verifier_gate = Some(Arc::clone(&gate));
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let control = tokio::time::timeout(
        Duration::from_secs(2),
        run_finish_phase(&ctx, &mut phase_state),
    )
    .await
    .expect("the verifier deadline must bound Finish");

    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(phase_state.run_failed);
    wait_for_completion_verifier_stop(&gate).await;
    let failed_plan = state.sessions.lock().await[&session_id]
        .pending_plan
        .clone()
        .expect("timed-out plan should remain resumable");
    assert_eq!(failed_plan.status, crate::plan::PlanStatus::Failed);
    assert_eq!(
        failed_plan.progress[0].status,
        crate::plan::PlanStepStatus::Blocked
    );
    assert!(
        failed_plan.progress[0]
            .note
            .contains("completion-contract-timeout")
    );
    let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
    let error = events
        .iter()
        .find(|event| {
            event["type"] == "error" && event["code"] == "plan_completion_contract_failed"
        })
        .expect("deadline should emit one stable contract failure");
    assert_eq!(error["run_terminal"].as_bool(), Some(true));
    assert_eq!(
        error["checks"][0]["check_id"],
        "completion-contract-timeout"
    );
    assert_eq!(
        error["checks"][0]["reason"],
        "completion verification exceeded its hard deadline"
    );
    assert!(events.iter().any(|event| {
        event["type"] == "done"
            && event["phase"] == "failed"
            && event["reason"] == "completion_contract_failed"
    }));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        live_rx.try_recv().is_err(),
        "no worker result may arrive late"
    );
    assert_eq!(
        state.sessions.lock().await[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Failed)
    );
}

#[tokio::test]
async fn run_finish_phase_completion_verifier_can_finish_normally_after_a_slow_start() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "finish-contract-normal-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let workspace = temp_workspace("finish-contract-normal");
    let _artifacts = RuntimeLoopTestArtifactsGuard::new(&session_id, &workspace);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let plan = completion_verifier_test_plan("plan_finish_contract_normal");
    let mut session = test_session(&session_id, "Finish contract normal", None);
    session.workspace = workspace.clone();
    session.working_directory = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("verify and finish this plan".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.pending_plan = Some(plan.clone());
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let gate = Arc::new(CompletionVerifierTestGate::default());
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::Execute;
    phase_state.cycle_workspace = workspace.clone();
    phase_state.approved_plan = Some(plan);
    phase_state.completion_evidence.verifier_gate = Some(Arc::clone(&gate));
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let trigger = async {
        wait_for_completion_verifier_gate(&gate).await;
        gate.release.store(true, Ordering::Relaxed);
    };
    let (control, ()) = tokio::join!(run_finish_phase(&ctx, &mut phase_state), trigger);

    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(!phase_state.run_failed);
    assert_eq!(
        state.sessions.lock().await[&session_id]
            .pending_plan
            .as_ref()
            .map(|plan| plan.status),
        Some(crate::plan::PlanStatus::Completed)
    );
    let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(!events.iter().any(|event| {
        event["type"] == "error" && event["code"] == "plan_completion_contract_failed"
    }));
    assert!(
        events
            .iter()
            .any(|event| { event["type"] == "done" && event["phase"] == "finish" })
    );
}

#[tokio::test]
async fn terminal_snapshot_is_frozen_before_a_delayed_dispatcher_and_next_input_save() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "terminal-source-freeze-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Terminal source freeze", None);
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("first run input".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(11),
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: Some("first run result".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(12),
    });
    let old_message_count = session.messages.len();
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    // Keep the receiver unread until after the next input is saved. This is
    // the production ordering that used to let the async dispatcher sample a
    // newer Session for the old terminal event.
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    assert!(matches!(
        run_finish_phase(&ctx, &mut phase_state).await,
        AgentPhaseControl::Break
    ));
    {
        let persist_gate = crate::session_store::session_persist_gate(&session_id);
        let _persist_guard = persist_gate.lock().await;
        let next_snapshot = {
            let mut sessions = state.sessions.lock().await;
            let session = sessions.get_mut(&session_id).expect("Session should exist");
            session.messages.push(ChatMessage {
                role: "user".into(),
                content: Some("next run input".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(13),
            });
            session.clone()
        };
        crate::session_store::save_session_to_disk_locked(&next_snapshot)
            .await
            .expect("next input should save without being overwritten");
    }

    let (terminal_snapshot, outcome) = take_terminal_persistence_record(&session_id)
        .await
        .expect("the terminal producer should freeze its own Session snapshot");
    assert_eq!(terminal_snapshot.messages.len(), old_message_count);
    assert_eq!(outcome.end_message_index + 1, old_message_count);
    assert!(
        terminal_snapshot
            .messages
            .iter()
            .all(|message| { message.content.as_deref() != Some("next run input") })
    );
    let persisted = crate::session_store::load_session_snapshot_from_path(
        &crate::session_store::sessions_dir().join(format!("{session_id}.json")),
    )
    .expect("the later Session save should remain intact");
    assert!(
        persisted
            .messages
            .iter()
            .any(|message| { message.content.as_deref() == Some("next run input") })
    );
    assert_eq!(live_rx.recv().await.unwrap()["type"], "done");

    crate::session_store::delete_session_from_storage(&session_id)
        .await
        .expect("test Session should be removed");
}

#[tokio::test]
async fn run_finish_phase_rolls_back_plan_registration_when_persistence_fails() {
    let state = Arc::new(test_app_state());
    let session_id = format!(
        "plan-finish-persist-failure-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Plan Finish Persist Failure", None);
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("Plan the persistence change".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "assistant".into(),
        content: Some("Draft plan response".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "submit-plan-persist-failure".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: crate::plan::TOOL_NAME_SUBMIT_PLAN.into(),
                arguments: "{}".into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    });
    let planning_plan = crate::PendingPlan {
        id: "plan_finish_persist_failure".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        revision: 1,
        status: crate::plan::PlanStatus::Planning,
        created_at: 10,
        updated_at: 10,
        initial_submission_pending: true,
        ..Default::default()
    };
    session.pending_plan = Some(planning_plan.clone());
    let original_messages = session.messages.clone();
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let failure_path = crate::session_store::sessions_dir().join(format!("{session_id}.json.tmp"));
    std::fs::create_dir_all(&failure_path).expect("failure sentinel directory should be created");

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.run_mode = AgentRunMode::PlanOnly;
    phase_state.plan_submission = Some(
        crate::plan::validate_submission_json(
            r#"{
                "state":"ready",
                "title":"Persist the plan safely",
                "goal":"Keep memory and durable state consistent",
                "steps":[{"id":"save","title":"Save the accepted revision"}],
                "acceptance_criteria":["The approved workspace remains available while the revision is saved."],
                "completion_checks":[{"id":"save-workspace","step_id":"save","covers":[{"section":"acceptance_criteria","index":0}],"kind":"workspace_path","path":".","expected_path_type":"directory"}]
            }"#,
        )
        .expect("submission should validate"),
    );
    phase_state
        .react_ctx
        .transition_to_finish(agent::FinishReason::Complete);

    let control = run_finish_phase(&ctx, &mut phase_state).await;

    std::fs::remove_dir_all(&failure_path).expect("failure sentinel should be removed");
    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(phase_state.run_failed);
    let sessions = state.sessions.lock().await;
    let session = &sessions[&session_id];
    let plan = session
        .pending_plan
        .as_ref()
        .expect("the planning plan should be restored for terminal handling");
    assert_eq!(plan.id, planning_plan.id);
    assert_eq!(plan.revision, planning_plan.revision);
    assert_eq!(plan.status, crate::plan::PlanStatus::Planning);
    assert_eq!(session.messages.len(), original_messages.len());
    assert_eq!(session.messages[2].content, original_messages[2].content);
    drop(sessions);
    let error = live_rx
        .try_recv()
        .expect("persistence failure should be reported");
    assert_eq!(error["type"].as_str(), Some("error"));
    assert!(
        live_rx.try_recv().is_err(),
        "an unsaved plan revision must not be announced"
    );
}

#[tokio::test]
async fn run_analyze_phase_emits_context_compress_skipped_for_low_savings() {
    let mut hooks = HookRegistry::new();
    hooks.register(Box::new(ForceCompressionSkippedHook));

    let state = Arc::new(test_app_state_with_hooks(hooks));
    install_openai_model(&state, "gpt-4o-reasoner", true);

    let session_id = "context-compress-skipped".to_string();
    let workspace = temp_workspace("context-compress-skipped");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", Some("openai/gpt-4o-reasoner"));
    session.workspace = workspace.clone();
    for idx in 0..14 {
        session.messages.push(ChatMessage {
            role: "user".into(),
            content: Some(format!("investigation turn {idx} {}", "C".repeat(900))),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        });
        session.messages.push(ChatMessage {
            role: "assistant".into(),
            content: Some(format!("analysis turn {idx} {}", "D".repeat(900))),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        });
    }
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.working_state.seed_from_query(Some(
        "investigate the timeout loop and explain the blockers",
    ));

    let analyze = run_analyze_phase(&ctx, &mut phase_state);
    tokio::pin!(analyze);

    let first = tokio::select! {
        event = live_rx.recv() => event.expect("first event should be emitted"),
        _ = &mut analyze => panic!("analyze phase ended before skipped event"),
    };
    assert_eq!(first["type"].as_str(), Some("context_compress_skipped"));
    assert_eq!(first["reason"].as_str(), Some("insufficient_savings"));

    run_cancel.cancel();
    let control = tokio::time::timeout(Duration::from_secs(2), analyze)
        .await
        .expect("analyze phase should stop promptly after cancellation");
    assert!(matches!(control, AgentPhaseControl::Break));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn run_hooks_skips_replace_messages_when_compression_saves_nothing() {
    let state = Arc::new(test_app_state());
    let session_id = "context-compress-no-savings".to_string();
    let workspace = temp_workspace("context-compress-no-savings");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", Some("openai/gpt-4o-reasoner"));
    session.workspace = workspace.clone();
    for idx in 0..14 {
        session.messages.push(ChatMessage {
            role: "user".into(),
            content: Some(format!("investigation turn {idx} {}", "C".repeat(900))),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        });
        session.messages.push(ChatMessage {
            role: "assistant".into(),
            content: Some(format!("analysis turn {idx} {}", "D".repeat(900))),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        });
    }
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session.clone());
    }

    let before_messages = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .expect("session should exist")
            .messages
            .clone()
    };
    let input_budget = 1;
    let input = crate::hooks::HookInput {
        messages: before_messages.clone(),
        model: "openai/gpt-4o-mini".into(),
        provider: Provider::OpenAI,
        workspace: workspace.clone(),
        input_budget,
        request_budget: None,
        compression_extra_tools: None,
        cycle: 0,
        compression_context: None,
    };

    let should_compress = crate::hooks::should_auto_compress(&input, 8, 90);
    assert!(should_compress);
    assert!(!crate::hooks::compression_saves_enough(10_000, 9_900));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn emit_events_usage_updates_session_tokens_for_skipped_compression() {
    let session_id = "context-compress-skipped-usage".to_string();
    let workspace = temp_workspace("context-compress-skipped-usage");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let state = Arc::new(test_app_state());
    let mut session = test_session(&session_id, "Main", Some("openai/gpt-4o-reasoner"));
    session.workspace = workspace.clone();
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let mut hooks = HookRegistry::new();
    hooks.register(Box::new(ForceCompressionSkippedHook));
    let events = run_hooks(
        &hooks,
        agent::HookPoint::BeforeAnalyze,
        &state.sessions,
        &session_id,
        &state.config(),
        &state.http,
        0,
        None,
        None,
        None,
    )
    .await;

    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"].as_str(), Some("context_compress_skipped"));

    let sessions = state.sessions.lock().await;
    let persisted = sessions.get(&session_id).expect("session should exist");
    assert_eq!(persisted.input_tokens, 123);
    assert_eq!(persisted.output_tokens, 45);
    assert_eq!(
        persisted.daily_provider_usage[&crate::context::usage_provider_label("openai")],
        [123, 45]
    );
    assert_eq!(
        persisted.daily_provider_usage
            [&crate::context::usage_role_label(crate::context::USAGE_ROLE_CONTEXT)],
        [123, 45]
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn run_analyze_phase_emits_context_pruned_after_before_analyze_compression() {
    let mut hooks = HookRegistry::new();
    hooks.register(Box::new(ForceCompressionHook));

    let state = Arc::new(test_app_state_with_hooks(hooks));
    install_openai_model(&state, "gpt-4o-reasoner", true);
    {
        let mut guard = state.config.lock().expect("config lock");
        let mut config = (**guard).clone();
        config.max_context_tokens = 1_000;
        if let Some(provider) = config.providers.get_mut("openai")
            && let Some(model) = provider.models.first_mut()
        {
            model.context_window = Some(1_000);
        }
        *guard = Arc::new(config);
    }

    let session_id = "context-compress-before-prune".to_string();
    let workspace = temp_workspace("context-compress-before-prune");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", Some("openai/gpt-4o-reasoner"));
    session.workspace = workspace.clone();
    for idx in 0..14 {
        session.messages.push(ChatMessage {
            role: "user".into(),
            content: Some(format!("investigation turn {idx} {}", "C".repeat(900))),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        });
        session.messages.push(ChatMessage {
            role: "assistant".into(),
            content: Some(format!("analysis turn {idx} {}", "D".repeat(900))),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        });
    }
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("investigate the timeout loop and explain the blockers".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    let terminal_message_anchor = RunMessageAnchor::from_messages(&session.messages)
        .expect("the direct Analyze fixture should capture its run-start message anchor");
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    phase_state.terminal_message_anchor = Some(terminal_message_anchor);
    phase_state.working_state.seed_from_query(Some(
        "investigate the timeout loop and explain the blockers",
    ));

    let control = run_analyze_phase(&ctx, &mut phase_state).await;
    assert!(matches!(control, AgentPhaseControl::Break));

    let compressed = live_rx
        .recv()
        .await
        .expect("compression event should be emitted");
    let pruned = live_rx.recv().await.expect("prune event should be emitted");
    let start = live_rx.recv().await.expect("start event should be emitted");
    let task_plan = live_rx.recv().await.expect("task plan should follow start");
    let auto_trace = live_rx.recv().await.expect("auto trace should be emitted");
    let error = live_rx
        .recv()
        .await
        .expect("budget error should stop the round");

    assert_eq!(compressed["type"].as_str(), Some("context_compressed"));
    assert_eq!(compressed["before_estimate"].as_u64(), Some(10_000));
    assert_eq!(compressed["after_estimate"].as_u64(), Some(4_000));
    assert_eq!(compressed["saved_tokens"].as_u64(), Some(6_000));
    assert_eq!(compressed["saved_percent"].as_u64(), Some(60));
    assert!(compressed["compression_ratio"].as_u64().is_some());
    assert_eq!(pruned["type"].as_str(), Some("context_pruned"));
    assert_eq!(start["type"].as_str(), Some("start"));
    assert_eq!(task_plan["type"].as_str(), Some("task_plan"));
    assert_eq!(auto_trace["type"].as_str(), Some("auto_trace"));
    assert!(pruned["messages_removed"].as_u64().unwrap_or(0) > 0);
    assert_eq!(error["type"].as_str(), Some("error"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn prepare_analyze_snapshot_applies_global_dynamic_budget_across_sections() {
    let state = Arc::new(test_app_state());
    let session_id = "dynamic-budget-session".to_string();
    let workspace = temp_workspace("dynamic-budget");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("investigate the timeout path and summarize the result".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        results_origin_query: None,
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: Some(format!("## Observation Hint\n{}", "A".repeat(1_400))),
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        unresolved_tool_targets: HashSet::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    };

    phase_state.working_state.seed_from_query(Some(
        "investigate the timeout path and summarize the result",
    ));
    for idx in 0..8 {
        phase_state
            .working_state
            .completed_steps
            .push(format!("completed step {idx}: {}", "C".repeat(180)));
        phase_state
            .working_state
            .evidence
            .push(agent::EvidenceItem {
                claim: format!("evidence item {idx}: {}", "D".repeat(180)),
                source_tool: "search_files".into(),
                source_ref: format!("src/runtime_loop_{idx}.rs"),
                confidence: agent::EvidenceConfidence::High,
            });
    }
    phase_state.working_state.open_questions = vec![
        format!("open question one: {}", "E".repeat(180)),
        format!("open question two: {}", "E".repeat(180)),
        format!("open question three: {}", "E".repeat(180)),
    ];
    phase_state.working_state.next_actions = vec![
        format!("next action one: {}", "F".repeat(180)),
        format!("next action two: {}", "F".repeat(180)),
        format!("next action three: {}", "F".repeat(180)),
    ];

    prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared");

    let prompt = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .and_then(|session| session.messages.first())
            .and_then(|message| message.content.clone())
            .expect("system prompt should be present")
    };

    assert!(prompt.contains("## Observation Hint"));
    assert!(prompt.contains("## Task State"));
    assert_eq!(prompt.matches(DYNAMIC_PROMPT_TRUNCATION_MARKER).count(), 1);
    assert!(!prompt.contains("## Working Method"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn prepare_analyze_snapshot_preserves_todos_when_optional_sections_overflow_budget() {
    let state = Arc::new(test_app_state());
    let session_id = "todos-required-dynamic-section".to_string();
    let workspace = temp_workspace("todos-required-dynamic-section");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let todo_id = "todo-timeout-path-anchor".to_string();
    let todo_content = format!("preserve this todo in prompt {}", "T".repeat(160));

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.todos = crate::todos::TodoSnapshot {
        revision: 7,
        items: vec![crate::todos::TodoItem {
            id: todo_id.clone(),
            content: todo_content.clone(),
            status: crate::todos::TodoStatus::InProgress,
        }],
        last_updated_by: crate::todos::TodoUpdatedBy::User,
        updated_at: now_epoch(),
    };
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("investigate the timeout path and preserve current work".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        results_origin_query: None,
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: Some(format!("## Observation Hint\n{}", "A".repeat(6_000))),
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        unresolved_tool_targets: HashSet::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    };

    prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared");

    let prompt = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .and_then(|session| session.messages.first())
            .and_then(|message| message.content.clone())
            .expect("system prompt should be present")
    };

    assert!(prompt.contains("## Current Todos"));
    assert!(prompt.contains("- revision: 7"));
    assert!(prompt.contains("- last_updated_by: user"));
    assert!(prompt.contains(
        "- note: the latest user edit is authoritative. Do not overwrite it from a stale plan."
    ));
    assert!(prompt.contains(&format!("id={}", serde_json::to_string(&todo_id).unwrap())));
    assert!(prompt.contains(&format!(
        "content={}",
        serde_json::to_string(&todo_content).unwrap()
    )));
    assert!(prompt.contains("## Observation Hint"));
    assert_eq!(prompt.matches(DYNAMIC_PROMPT_TRUNCATION_MARKER).count(), 1);
    assert!(
        prompt
            .find("## Current Todos")
            .zip(prompt.find("## Observation Hint"))
            .is_some_and(|(todos_idx, obs_idx)| todos_idx < obs_idx)
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn prepare_analyze_snapshot_resets_runtime_auto_state_for_new_goal() {
    let state = Arc::new(test_app_state());
    let session_id = "snapshot-new-goal-reset".to_string();
    let workspace = temp_workspace("snapshot-new-goal-reset");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("investigate the timeout path".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("fix the parser instead".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        results_origin_query: None,
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: Some("## Observation Hint\nlegacy observation context".into()),
        last_observation_strength: agent::AutoObservationStrength::Strong,
        last_tool_results_count: 3,
        last_tool_error_count: 2,
        last_summary_count: 1,
        last_summary_bytes: 4096,
        last_progress_made: true,
        last_error_kind: agent::AutoErrorKind::Timeout,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::NoMeaningfulProgress,
        stagnation_streak: 4,
        error_streak: 3,
        unresolved_tool_targets: HashSet::new(),
        recent_tool_history: vec![agent::ToolResultEntry {
            id: "tool-1".into(),
            name: "read_file".into(),
            duration_ms: 7,
            is_error: true,
            result: "timed out".into(),
            call_summary: Some("read `src/runtime_loop.rs`".into()),
            trace: None,
        }],
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    };
    phase_state
        .working_state
        .seed_from_query(Some("investigate the timeout path"));
    phase_state.react_ctx.cycles = 6;

    prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared");

    assert_eq!(
        phase_state.working_state.primary_goal.as_deref(),
        Some("fix the parser instead")
    );
    assert_eq!(phase_state.working_state.intent, agent::TaskIntent::Change);
    assert_eq!(
        phase_state.last_observation_strength,
        agent::AutoObservationStrength::None
    );
    assert_eq!(phase_state.last_tool_results_count, 0);
    assert_eq!(phase_state.last_tool_error_count, 0);
    assert_eq!(phase_state.last_summary_count, 0);
    assert_eq!(phase_state.last_summary_bytes, 0);
    assert!(!phase_state.last_progress_made);
    assert_eq!(phase_state.last_error_kind, agent::AutoErrorKind::None);
    assert_eq!(
        phase_state.last_evidence_delta_quality,
        agent::AutoEvidenceDeltaQuality::None
    );
    assert_eq!(phase_state.stagnation_streak, 0);
    assert_eq!(phase_state.error_streak, 0);
    assert_eq!(phase_state.react_ctx.cycles, 0);
    assert!(phase_state.recent_tool_history.is_empty());

    let prompt = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .and_then(|session| session.messages.first())
            .and_then(|message| message.content.clone())
            .expect("system prompt should be present")
    };
    assert!(!prompt.contains("legacy observation context"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn update_working_state_keeps_results_attached_to_their_original_query() {
    let state = Arc::new(test_app_state());
    let session_id = "result-query-session".to_string();
    let mut session = test_session(&session_id, "Main", None);
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("inspect the timeout wiring".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("benchmark the timeout path instead".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: vec![agent::ToolResultEntry {
            id: "c1".into(),
            name: "read_file".into(),
            result: "timeout_ms = 45".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("read `src/runtime.rs`".into()),
            trace: None,
        }],
        results_origin_query: Some("inspect the timeout wiring".into()),
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: None,
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        unresolved_tool_targets: HashSet::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    };

    update_working_state(&ctx, &mut phase_state, &[]).await;

    assert_eq!(
        phase_state.working_state.primary_goal.as_deref(),
        Some("inspect the timeout wiring")
    );
    assert_eq!(
        phase_state.working_state.intent,
        agent::TaskIntent::Investigate
    );
    assert!(phase_state.working_state.evidence.iter().any(|item| {
        item.claim.contains("Observed file content") && item.claim.contains("timeout_ms = 45")
    }));
}

#[tokio::test]
async fn update_working_state_reuses_same_cycle_task_memory_selection() {
    let state = Arc::new(test_app_state());
    {
        let mut guard = state.config.lock().expect("config lock");
        let mut config = (**guard).clone();
        config.structured_memory = true;
        *guard = Arc::new(config);
    }

    let session_id = "same-cycle-task-memory-session".to_string();
    let workspace = temp_workspace("same-cycle-task-memory");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);
    memory::save_structured_memory(
        &workspace,
        &memory::StructuredMemory {
            lessons: vec![memory::MemoryLesson {
                title: "Lesson A".into(),
                recommendation: "Keep the original memory selection".into(),
                confidence: memory::MemoryConfidence::High,
                last_seen_at: 10,
                ..memory::MemoryLesson::default()
            }],
            ..memory::StructuredMemory::default()
        },
    )
    .expect("structured memory should save");

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("fix the cargo workspace test flow".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        results_origin_query: None,
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: None,
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        unresolved_tool_targets: HashSet::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    };

    prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared");
    let first_retrieved = phase_state
        .retrieved_task_memory
        .clone()
        .expect("task memory should be available");
    assert!(
        first_retrieved
            .lessons
            .iter()
            .any(|lesson| lesson.title == "Lesson A")
    );

    memory::save_structured_memory(
        &workspace,
        &memory::StructuredMemory {
            lessons: vec![memory::MemoryLesson {
                title: "Lesson B".into(),
                recommendation: "This should not replace the same-cycle cache".into(),
                confidence: memory::MemoryConfidence::High,
                last_seen_at: 20,
                ..memory::MemoryLesson::default()
            }],
            ..memory::StructuredMemory::default()
        },
    )
    .expect("structured memory should save");

    update_working_state(&ctx, &mut phase_state, &[]).await;

    let reused = phase_state
        .retrieved_task_memory
        .as_ref()
        .expect("task memory should still be available");
    assert!(
        reused
            .lessons
            .iter()
            .any(|lesson| lesson.title == "Lesson A")
    );
    assert!(
        !reused
            .lessons
            .iter()
            .any(|lesson| lesson.title == "Lesson B")
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn update_working_state_refreshes_task_memory_after_state_changes() {
    let state = Arc::new(test_app_state());
    {
        let mut guard = state.config.lock().expect("config lock");
        let mut config = (**guard).clone();
        config.structured_memory = true;
        *guard = Arc::new(config);
    }

    let session_id = "post-update-task-memory-session".to_string();
    let workspace = temp_workspace("post-update-task-memory");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);
    memory::save_structured_memory(
        &workspace,
        &memory::StructuredMemory {
            lessons: vec![
                memory::MemoryLesson {
                    title: "Entrypoint wiring".into(),
                    recommendation: "Inspect src/main.rs first".into(),
                    confidence: memory::MemoryConfidence::High,
                    last_seen_at: 10,
                    ..memory::MemoryLesson::default()
                },
                memory::MemoryLesson {
                    title: "Timeout source".into(),
                    recommendation: "src/runtime.rs owns timeout_ms".into(),
                    confidence: memory::MemoryConfidence::High,
                    last_seen_at: 20,
                    ..memory::MemoryLesson::default()
                },
            ],
            ..memory::StructuredMemory::default()
        },
    )
    .expect("structured memory should save");

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("inspect the entrypoint wiring".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: vec![agent::ToolResultEntry {
            id: "c1".into(),
            name: "read_file".into(),
            result: "timeout_ms = 45".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("read `src/runtime.rs`".into()),
            trace: Some(agent::ToolExecutionTrace {
                summary: "read `src/runtime.rs`".into(),
                path: Some("src/runtime.rs".into()),
                ..agent::ToolExecutionTrace::default()
            }),
        }],
        results_origin_query: Some("inspect the entrypoint wiring".into()),
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: None,
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        unresolved_tool_targets: HashSet::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    };

    update_working_state(&ctx, &mut phase_state, &[]).await;

    let refreshed = phase_state
        .retrieved_task_memory
        .as_ref()
        .expect("task memory should still be available");
    assert!(
        refreshed
            .lessons
            .iter()
            .any(|lesson| lesson.title == "Timeout source")
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn prepare_analyze_snapshot_injects_fresh_task_state_each_time() {
    let state = Arc::new(test_app_state());
    let session_id = "task-state-session".to_string();
    let workspace = temp_workspace("task-state");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("inspect the timeout wiring".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        results_origin_query: None,
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: None,
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        unresolved_tool_targets: HashSet::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    };

    phase_state
        .working_state
        .seed_from_query(Some("inspect the timeout wiring"));
    phase_state
        .working_state
        .completed_steps
        .push("step alpha".into());
    prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared");

    let first_prompt = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .and_then(|session| session.messages.first())
            .and_then(|message| message.content.clone())
            .expect("system prompt should be present")
    };
    assert!(first_prompt.contains("## Task State"));
    assert!(first_prompt.contains("step alpha"));

    phase_state.working_state.completed_steps.clear();
    phase_state
        .working_state
        .completed_steps
        .push("step beta".into());
    prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared again");

    let second_prompt = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .and_then(|session| session.messages.first())
            .and_then(|message| message.content.clone())
            .expect("system prompt should be present")
    };
    assert!(second_prompt.contains("step beta"));
    assert!(!second_prompt.contains("step alpha"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn prepare_analyze_snapshot_injects_retrieved_task_memory() {
    let state = Arc::new(test_app_state());
    {
        let mut guard = state.config.lock().expect("config lock");
        let mut config = (**guard).clone();
        config.structured_memory = true;
        *guard = Arc::new(config);
    }

    let session_id = "task-memory-session".to_string();
    let workspace = temp_workspace("task-memory");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);

    memory::save_structured_memory(
        &workspace,
        &memory::StructuredMemory {
            lessons: vec![memory::MemoryLesson {
                title: "Rust test loop".into(),
                when_to_apply: "before a full workspace pass".into(),
                recommendation: "Run cargo check first".into(),
                scope: "repo".into(),
                confidence: memory::MemoryConfidence::High,
                last_seen_at: 10,
            }],
            open_loops: vec![memory::OpenLoop {
                goal: "stabilize workspace tests".into(),
                blocker: "command choice is inconsistent".into(),
                next_step: "standardize on cargo test --workspace".into(),
                status: memory::OpenLoopStatus::Open,
                updated_at: 20,
            }],
            project_signals: vec![memory::ProjectSignal {
                key: "test_command".into(),
                value: "cargo test --workspace".into(),
                recorded_at: 30,
            }],
            command_patterns: vec![memory::CommandPattern {
                signature: "cargo test --workspace".into(),
                purpose: "validate the Rust workspace".into(),
                outcome: "full regression signal".into(),
                confidence: memory::MemoryConfidence::High,
                last_seen_at: 40,
            }],
            ..memory::StructuredMemory::default()
        },
    )
    .expect("structured memory should save");

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("fix the cargo workspace test flow".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        results_origin_query: None,
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: None,
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        unresolved_tool_targets: HashSet::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    };

    prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared");

    let prompt = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .and_then(|session| session.messages.first())
            .and_then(|message| message.content.clone())
            .expect("system prompt should be present")
    };

    assert!(prompt.contains("## Relevant Past Experience"));
    assert!(prompt.contains("Run cargo check first"));
    assert!(prompt.contains("stabilize workspace tests"));
    assert!(prompt.contains("## Tool Hints"));
    assert!(prompt.contains("Prefer `exec`"));
    assert!(prompt.contains("## Suggested Tool Order"));
    assert!(prompt.contains("1. **exec**"));
    assert!(phase_state.retrieved_task_memory.is_some());

    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn prepare_analyze_snapshot_injects_agent_recommendations_and_delegation_guidance() {
    let state = Arc::new(test_app_state());
    let session_id = "delegation-guidance-session".to_string();
    let workspace = temp_workspace("delegation-guidance");
    std::fs::create_dir_all(workspace.join("agents/reviewer"))
        .expect("reviewer agent dir should be created");
    std::fs::create_dir_all(workspace.join("agents/benchmarker"))
        .expect("benchmarker agent dir should be created");
    prompts::init_session_prompt_files(&workspace);
    std::fs::write(
        workspace.join("agents/reviewer/AGENT.md"),
        "---\nname: reviewer\ndescription: \"Code review and debugging specialist\"\n---\n\nReview code and debug failures.\n",
    )
    .expect("reviewer agent should be written");
    std::fs::write(
        workspace.join("agents/benchmarker/AGENT.md"),
        "---\nname: benchmarker\ndescription: \"Benchmark and performance profiling specialist\"\n---\n\nProfile slow paths and compare regressions.\n",
    )
    .expect("benchmarker agent should be written");

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.messages.push(ChatMessage {
        role: "user".into(),
        content: Some("debug the failing tests and profile the runtime timeout path".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    });

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        results_origin_query: None,
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: None,
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        unresolved_tool_targets: HashSet::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    };

    phase_state.working_state.seed_from_query(Some(
        "debug the failing tests and profile the runtime timeout path",
    ));
    phase_state
        .working_state
        .next_actions
        .push("Inspect the first failing workspace test.".into());
    phase_state
        .working_state
        .next_actions
        .push("Profile the timeout path in the runtime loop.".into());

    prepare_analyze_snapshot(&ctx, &mut phase_state)
        .await
        .expect("snapshot should be prepared");

    let prompt = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .and_then(|session| session.messages.first())
            .and_then(|message| message.content.clone())
            .expect("system prompt should be present")
    };

    assert!(prompt.contains("## Suggested Sub-Agents"));
    assert!(prompt.contains("**reviewer**"));
    assert!(prompt.contains("**benchmarker**"));
    assert!(prompt.contains("## Delegation Guidance"));
    assert!(prompt.contains("Prefer `orchestrate`"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn build_working_state_digest_user_prompt_includes_retrieved_memory() {
    let mut state = agent::WorkingState::default();
    state.seed_from_query(Some("fix the cargo workspace test flow"));
    let task_memory = memory::RetrievedTaskMemory {
        lessons: vec![memory::MemoryLesson {
            title: "Rust test loop".into(),
            when_to_apply: "before a full workspace pass".into(),
            recommendation: "Run cargo check first".into(),
            scope: "repo".into(),
            confidence: memory::MemoryConfidence::High,
            last_seen_at: 10,
        }],
        ..memory::RetrievedTaskMemory::default()
    };
    let prompt = build_working_state_digest_user_prompt(
        &state,
        Some("fix the cargo workspace test flow"),
        &[],
        &[agent::ToolResultEntry {
            id: "c1".into(),
            name: "exec".into(),
            result: "ok".into(),
            duration_ms: 3,
            is_error: false,
            call_summary: Some("run `cargo test --workspace`".into()),
            trace: None,
        }],
        Some(&task_memory),
    )
    .expect("prompt should build");

    assert!(prompt.contains("Relevant past experience"));
    assert!(prompt.contains("Rust test loop"));
    assert!(prompt.contains("Run cargo check first"));
    assert!(prompt.contains("Call: run `cargo test --workspace`"));
}

#[test]
fn summarize_effective_tool_args_formats_exec_and_read_file_context() {
    let exec = summarize_effective_tool_args(
        "exec",
        Some(r#"{"command":"cargo test --workspace","working_dir":"crates/core"}"#),
    )
    .expect("exec summary should render");
    assert_eq!(exec, "run `cargo test --workspace` in `crates/core`");

    let read = summarize_effective_tool_args(
        "read_file",
        Some(r#"{"path":"src/main.rs","start_line":10,"end_line":40}"#),
    )
    .expect("read_file summary should render");
    assert_eq!(read, "read `src/main.rs` lines 10-40");
}

#[tokio::test]
async fn apply_llm_response_persists_multi_tool_assistant_with_thinking() {
    let state = Arc::new(test_app_state());
    let session_id = "deepseek-session".to_string();
    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            session_id.clone(),
            test_session(&session_id, "DeepSeek", None),
        );
    }

    let ctx = AgentRunCtx {
        state: &state,
        config: state.config(),
        model: state.config().model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        results_origin_query: None,
        working_state: agent::WorkingState::default(),
        run_mode: AgentRunMode::Execute,
        task_plan: None,
        retrieved_task_memory: None,
        retrieved_task_memory_key: None,
        retrieved_task_memory_cycle: None,
        cycle_workspace: PathBuf::new(),
        session_home: PathBuf::new(),
        last_observation_hint: None,
        last_observation_strength: agent::AutoObservationStrength::None,
        last_tool_results_count: 0,
        last_tool_error_count: 0,
        last_summary_count: 0,
        last_summary_bytes: 0,
        last_progress_made: false,
        last_error_kind: agent::AutoErrorKind::None,
        last_evidence_delta_quality: agent::AutoEvidenceDeltaQuality::None,
        stagnation_streak: 0,
        error_streak: 0,
        recent_tool_history: Vec::new(),
        unresolved_tool_targets: HashSet::new(),
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        terminal_message_anchor: None,
        terminal_plan_identity: None,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
        plan_submission: None,
        plan_text_fallback_used: false,
        plan_evidence: Vec::new(),
        plan_evidence_truncated: false,
        replace_plan_evidence: false,
        approved_plan: None,
        completion_evidence: PlanCompletionRunEvidence::default(),
        plan_action_prompt: None,
    };

    let response = providers::LlmResponse {
        message: ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: Some("plan both files".into()),
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "call_1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: r#"{"path":"README.md"}"#.into(),
                    },
                },
                ToolCall {
                    id: "call_2".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: r#"{"path":"Cargo.toml"}"#.into(),
                    },
                },
            ]),
            tool_call_id: None,
            timestamp: None,
        },
        input_tokens: Some(123),
        output_tokens: Some(45),
        tool_image_compatibility_fallback: false,
    };

    apply_llm_response(
        &ctx,
        &mut phase_state,
        Provider::OpenAI,
        "deepseek".to_string(),
        crate::context::USAGE_ROLE_PRIMARY,
        123,
        None,
        response,
    )
    .await;

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&session_id)
        .expect("session should be persisted");
    let saved = session
        .messages
        .last()
        .expect("assistant message should be appended");

    assert_eq!(saved.role, "assistant");
    assert_eq!(saved.thinking.as_deref(), Some("plan both files"));
    assert!(saved.content.is_none());
    let tool_calls = saved
        .tool_calls
        .as_ref()
        .expect("assistant tool calls should be saved");
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[1].id, "call_2");
    assert_eq!(phase_state.pending_tool_calls.len(), 2);
    assert_eq!(phase_state.pending_tool_calls[0].id, "call_1");
    assert_eq!(phase_state.pending_tool_calls[1].id, "call_2");
}

fn test_s3_config() -> S3Config {
    S3Config {
        endpoint: "https://minio.example.test/storage".to_string(),
        region: "us-east-1".to_string(),
        bucket: "bucket".to_string(),
        access_key: "access-key".to_string(),
        secret_key: "secret-key".to_string(),
        prefix: "images/".to_string(),
        url_expiry_secs: 3600,
        lifecycle_days: 14,
    }
}

fn test_image_attachment() -> ImageAttachment {
    ImageAttachment {
        url: "https://example.test/image.png".to_string(),
        name: None,
        mime_type: None,
        s3_object_key: None,
        s3_config_id: None,
        cache_path: None,
        data: None,
    }
}

#[tokio::test]
async fn canceled_tool_image_upload_records_text_result_before_breaking() {
    let mut config = test_config();
    config.model = "openai/vision-model".to_string();
    config.s3 = Some(test_s3_config());
    config.providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "vision-model".to_string(),
                input: Some(vec!["text".to_string(), "image".to_string()]),
                ..Default::default()
            }],
        },
    );
    let state = Arc::new(test_app_state_with_config(config));
    let session_id = "tool-image-cancel".to_string();
    state.sessions.lock().await.insert(
        session_id.clone(),
        test_session(&session_id, "Tool Image Cancel", None),
    );

    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    run_cancel.cancel();
    let (live_tx, live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    drop(live_rx);
    let config = state.config();
    let ctx = AgentRunCtx {
        state: &state,
        config: Arc::clone(&config),
        model: config.model.clone(),
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = phase_state_for_analyze_test();
    let tool_call = ToolCall {
        id: "call-image-cancel".to_string(),
        call_type: "function".to_string(),
        gemini_thought_signature: None,
        function: FunctionCall {
            name: tools::TOOL_NAME_VIEW_IMAGE.to_string(),
            arguments: r#"{"path":"chart.png"}"#.to_string(),
        },
    };
    let result = tools::ToolOutcome {
        output: "Image read successfully.".to_string(),
        is_error: false,
        duration_ms: 12,
        subagent_snapshot: None,
        images: vec![tools::ToolImageOutput {
            data: vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
            mime_type: "image/png".to_string(),
            name: "chart.png".to_string(),
        }],
    };

    let control = record_tool_result(
        &ctx,
        &mut phase_state,
        &tool_call,
        result,
        None,
        PlanEvidenceCapture::default(),
    )
    .await;

    assert!(matches!(control, AgentPhaseControl::Break));
    assert!(phase_state.run_detached);
    assert_eq!(phase_state.collected_results.len(), 1);
    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_id).expect("session should exist");
    let persisted = session
        .messages
        .last()
        .expect("completed tool result should be persisted");
    assert_eq!(persisted.role, "tool");
    assert_eq!(persisted.tool_call_id.as_deref(), Some("call-image-cancel"));
    assert!(persisted.images.is_none());
    let content = persisted.content.as_deref().unwrap_or_default();
    assert!(content.contains("Image read successfully."));
    assert!(content.contains("run ended before upload completed"));
    assert_eq!(session.tool_calls_count, 1);
}

#[test]
fn select_analyze_model_keeps_primary_for_image_turn_when_fast_lacks_image_support() {
    let mut config = test_config();
    config.model = "openai/gpt-4o".to_string();
    config.fast_model = Some("openai/gpt-4o-mini".to_string());
    config.providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![
                JsonModelEntry {
                    id: "gpt-4o".to_string(),
                    input: Some(vec!["text".to_string(), "image".to_string()]),
                    ..Default::default()
                },
                JsonModelEntry {
                    id: "gpt-4o-mini".to_string(),
                    input: Some(vec!["text".to_string()]),
                    ..Default::default()
                },
            ],
        },
    );

    let (model, role) = select_analyze_model(
        &config,
        &config.model,
        config.fast_model.as_deref(),
        0,
        false,
        Some("鏉╂瑥绱堕崶楣冨櫡閺勵垯绮堟稊鍫吹"),
        true,
    );

    assert_eq!(model, "openai/gpt-4o");
    assert_eq!(role, crate::context::USAGE_ROLE_PRIMARY);
}

#[test]
fn select_analyze_model_uses_fast_for_simple_text_turn() {
    let mut config = test_config();
    config.model = "openai/gpt-4o".to_string();
    config.fast_model = Some("openai/gpt-4o-mini".to_string());

    let (model, role) = select_analyze_model(
        &config,
        &config.model,
        config.fast_model.as_deref(),
        0,
        false,
        Some("hello"),
        false,
    );

    assert_eq!(model, "openai/gpt-4o-mini");
    assert_eq!(role, crate::context::USAGE_ROLE_FAST);
}

#[test]
fn tool_image_capability_follows_primary_consumer_after_fast_tool_call() {
    let mut config = test_config();
    config.model = "openai/gpt-4o".to_string();
    config.fast_model = Some("openai/gpt-4o-mini".to_string());
    config.s3 = Some(test_s3_config());
    config.providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![
                JsonModelEntry {
                    id: "gpt-4o".to_string(),
                    input: Some(vec!["text".to_string(), "image".to_string()]),
                    ..Default::default()
                },
                JsonModelEntry {
                    id: "gpt-4o-mini".to_string(),
                    input: Some(vec!["text".to_string()]),
                    ..Default::default()
                },
            ],
        },
    );

    let (tool_call_model, _) = select_analyze_model(
        &config,
        &config.model,
        config.fast_model.as_deref(),
        0,
        false,
        Some("hello"),
        false,
    );

    assert_eq!(tool_call_model, "openai/gpt-4o-mini");
    assert!(!config.model_supports_image(&tool_call_model));
    assert!(tool_images_available_for_consumer(
        &config,
        &config.model,
        false
    ));
    assert!(!tool_images_available_for_consumer(
        &config,
        &config.model,
        true
    ));

    config.providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![
                JsonModelEntry {
                    id: "gpt-4o".to_string(),
                    input: Some(vec!["text".to_string()]),
                    ..Default::default()
                },
                JsonModelEntry {
                    id: "gpt-4o-mini".to_string(),
                    input: Some(vec!["text".to_string(), "image".to_string()]),
                    ..Default::default()
                },
            ],
        },
    );

    let (tool_call_model, _) = select_analyze_model(
        &config,
        &config.model,
        config.fast_model.as_deref(),
        0,
        false,
        Some("hello"),
        false,
    );
    assert_eq!(tool_call_model, "openai/gpt-4o-mini");
    assert!(config.model_supports_image(&tool_call_model));
    assert!(!tool_images_available_for_consumer(
        &config,
        &config.model,
        false
    ));
}

#[test]
fn select_analyze_model_uses_fast_for_image_turn_when_fast_supports_images() {
    let mut config = test_config();
    config.model = "openai/gpt-4o".to_string();
    config.fast_model = Some("openai/gpt-4o-mini".to_string());
    config.providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![
                JsonModelEntry {
                    id: "gpt-4o".to_string(),
                    input: Some(vec!["text".to_string(), "image".to_string()]),
                    ..Default::default()
                },
                JsonModelEntry {
                    id: "gpt-4o-mini".to_string(),
                    input: Some(vec!["text".to_string(), "image".to_string()]),
                    ..Default::default()
                },
            ],
        },
    );

    let (model, role) = select_analyze_model(
        &config,
        &config.model,
        config.fast_model.as_deref(),
        0,
        false,
        Some("what is in this image"),
        true,
    );

    assert_eq!(model, "openai/gpt-4o-mini");
    assert_eq!(role, crate::context::USAGE_ROLE_FAST);
}

#[test]
fn messages_have_images_detects_historical_image_context() {
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("system".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("previous image".into()),
            images: Some(vec![test_image_attachment()]),
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    assert!(messages_have_images(&messages));
}

#[test]
fn drain_busy_socket_messages_collects_interventions_and_stops_run() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = std::sync::Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let (inbound_tx, mut inbound_rx) = mpsc::channel(8);
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let run_cancel = CancellationToken::new();
    let mut pending = Vec::new();

    rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .insert(session_id.clone(), test_session(&session_id, "Main", None));
        inbound_tx
            .send("follow-up detail".to_string())
            .await
            .expect("first message should be queued");
        inbound_tx
            .send("/help".to_string())
            .await
            .expect("command should be queued");
        inbound_tx
            .send("/stop".to_string())
            .await
            .expect("stop should be queued");

        let stopped = drain_busy_socket_messages(
            &state,
            &session_id,
            &mut inbound_rx,
            &mut pending,
            &live_tx,
            &run_cancel,
        )
        .await;

        assert!(stopped);
    });

    assert!(run_cancel.is_cancelled());
    assert_eq!(pending, vec!["follow-up detail".to_string()]);

    let progress_event = live_rx
        .try_recv()
        .expect("progress event should be emitted");
    assert_eq!(progress_event["type"], "progress");
    assert!(
        progress_event["content"]
            .as_str()
            .is_some_and(|value| value.contains("Intervention received"))
    );
    assert!(live_rx.try_recv().is_err());
}

#[test]
fn drain_busy_socket_messages_applies_think_command_without_queueing_it() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = std::sync::Arc::new(test_app_state_with_config(test_reasoning_config()));
    let session_id = MAIN_SESSION_ID.to_string();
    let (inbound_tx, mut inbound_rx) = mpsc::channel(8);
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let (configuration_tx, mut configuration_rx) = mpsc::channel::<String>(8);
    let run_cancel = CancellationToken::new();
    let mut pending = Vec::new();

    rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .insert(session_id.clone(), test_session(&session_id, "Main", None));
        state.session_clients.lock().await.insert(
            session_id.clone(),
            SessionClientBinding {
                connection_id: 1,
                tx: configuration_tx,
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );

        inbound_tx
            .send("/think high".to_string())
            .await
            .expect("think command should be queued");
        inbound_tx
            .send("follow-up detail".to_string())
            .await
            .expect("intervention should be queued");

        let stopped = drain_busy_socket_messages(
            &state,
            &session_id,
            &mut inbound_rx,
            &mut pending,
            &live_tx,
            &run_cancel,
        )
        .await;

        assert!(!stopped);
    });

    assert_eq!(pending, vec!["follow-up detail".to_string()]);
    assert!(!run_cancel.is_cancelled());

    let think_event = live_rx.try_recv().expect("think event should be emitted");
    assert_eq!(think_event["type"], "system");
    assert!(
        think_event["content"]
            .as_str()
            .is_some_and(|value| value.contains("Think mode set to: high"))
    );
    assert!(
        think_event["content"]
            .as_str()
            .is_some_and(|value| value.contains("next reasoning cycle"))
    );

    let session_event = live_rx.try_recv().expect("session event should be emitted");
    assert_eq!(session_event["type"], "session");

    let progress_event = live_rx
        .try_recv()
        .expect("progress event should be emitted");
    assert_eq!(progress_event["type"], "progress");
    assert!(
        progress_event["content"]
            .as_str()
            .is_some_and(|value| value.contains("Intervention received"))
    );
    assert!(live_rx.try_recv().is_err());

    let configuration_event: serde_json::Value = serde_json::from_str(
        &configuration_rx
            .try_recv()
            .expect("busy think should broadcast model configuration"),
    )
    .expect("model configuration broadcast should be valid JSON");
    assert_eq!(configuration_event["type"], "session_model_configuration");
    assert_eq!(configuration_event["id"], MAIN_SESSION_ID);
    assert_eq!(configuration_event["effort"], "high");
    assert!(configuration_event["configRevision"].as_u64().is_some());
    assert!(configuration_rx.try_recv().is_err());

    let updated_think = rt.block_on(async {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .expect("session should exist")
            .think_level
            .clone()
    });
    assert_eq!(updated_think, "high");
}

#[test]
fn drain_busy_socket_messages_marks_think_failures_nonterminal_and_keeps_run_active() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let mut config = test_config();
    config.explicit_primary_model_configured = false;
    let state = std::sync::Arc::new(test_app_state_with_config(config));
    let session_id = MAIN_SESSION_ID.to_string();
    let (inbound_tx, mut inbound_rx) = mpsc::channel(8);
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let run_cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(false));
    let deferred_interventions = Arc::new(Mutex::new(DeferredInterventionState::open()));
    let mut pending = Vec::new();

    rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .insert(session_id.clone(), test_session(&session_id, "Main", None));
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: run_cancel.clone(),
                stop_requested: Arc::clone(&stop_requested),
                deferred_interventions: Arc::clone(&deferred_interventions),
            },
        );
        inbound_tx
            .send("/think high".to_string())
            .await
            .expect("think command should be queued");
        inbound_tx
            .send("continue after the command failure".to_string())
            .await
            .expect("intervention should be queued");

        let stopped = drain_busy_socket_messages(
            &state,
            &session_id,
            &mut inbound_rx,
            &mut pending,
            &live_tx,
            &run_cancel,
        )
        .await;

        assert!(!stopped);
        assert!(state.active_runs.lock().await.contains_key(&session_id));
    });

    assert!(!run_cancel.is_cancelled());
    assert!(!stop_requested.load(Ordering::Relaxed));
    assert_eq!(
        pending,
        vec!["continue after the command failure".to_string()]
    );

    let error_event = live_rx
        .try_recv()
        .expect("failed busy command should emit an error");
    assert_eq!(error_event["type"], "error");
    assert_eq!(error_event["run_terminal"], false);
    assert!(
        error_event["content"]
            .as_str()
            .is_some_and(|value| value.contains("Failed to persist think level"))
    );

    let progress_event = live_rx
        .try_recv()
        .expect("the following intervention should still be accepted");
    assert_eq!(progress_event["type"], "progress");
    assert!(live_rx.try_recv().is_err());
}

#[test]
fn persist_pending_interventions_appends_user_messages() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = std::sync::Arc::new(test_app_state());
    let mut session = test_session("session-a", "Session A", None);
    session.updated_at = 1;
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan_stale".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        created_at: 10,
        ..Default::default()
    });
    session.version = SESSION_VERSION;

    rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .insert(session.id.clone(), session.clone());
    });

    let mut pending = vec!["first note".to_string(), "second note".to_string()];
    let changed = rt.block_on(persist_pending_interventions(
        &state,
        "session-a",
        &mut pending,
    ));

    assert!(changed);
    assert!(pending.is_empty());

    let persisted = rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .get("session-a")
            .cloned()
            .expect("session should still exist")
    });
    assert_eq!(persisted.messages.len(), 3);
    assert_eq!(persisted.messages[1].role, "user");
    assert_eq!(persisted.messages[1].content.as_deref(), Some("first note"));
    assert_eq!(
        persisted.messages[2].content.as_deref(),
        Some("second note")
    );
    assert_eq!(
        persisted.pending_plan.as_ref().map(|plan| plan.id.as_str()),
        Some("plan_stale")
    );
    assert!(persisted.updated_at >= 1);

    let saved = crate::session_store::load_session_from_disk("session-a")
        .expect("session should be saved to disk");
    assert_eq!(
        saved.pending_plan.as_ref().map(|plan| plan.id.as_str()),
        Some("plan_stale")
    );
    assert_eq!(saved.messages.len(), 3);
    let _ = std::fs::remove_file(crate::session_store::sessions_dir().join("session-a.json"));
}

#[test]
fn persist_pending_interventions_keeps_messages_when_session_is_missing() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = std::sync::Arc::new(test_app_state());
    let mut pending = vec!["follow-up detail".to_string()];

    let changed = rt.block_on(persist_pending_interventions(
        &state,
        "missing-session",
        &mut pending,
    ));

    assert!(!changed);
    assert_eq!(pending, vec!["follow-up detail".to_string()]);
}

#[test]
fn persist_pending_interventions_rolls_back_when_the_save_fails() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = std::sync::Arc::new(test_app_state());
    let session_id = format!(
        "intervention-persist-failure-{}",
        crate::generate_random_session_id().expect("random session id")
    );
    let mut session = test_session(&session_id, "Intervention Persist Failure", None);
    session.updated_at = 1;
    let original = session.clone();
    rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .insert(session_id.clone(), session);
    });

    let failure_path = crate::session_store::sessions_dir().join(format!("{session_id}.json.tmp"));
    std::fs::create_dir_all(&failure_path).expect("failure sentinel directory should be created");
    let mut pending = vec!["must remain pending".to_string()];

    let changed = rt.block_on(persist_pending_interventions(
        &state,
        &session_id,
        &mut pending,
    ));

    std::fs::remove_dir_all(&failure_path).expect("failure sentinel should be removed");
    assert!(!changed);
    assert_eq!(pending, vec!["must remain pending".to_string()]);
    let restored = rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .expect("session should remain loaded")
    });
    assert_eq!(restored.messages.len(), original.messages.len());
    assert_eq!(restored.updated_at, original.updated_at);
    assert_eq!(
        restored.pending_plan.as_ref().map(|plan| plan.id.as_str()),
        original.pending_plan.as_ref().map(|plan| plan.id.as_str())
    );
}

#[test]
fn hard_cap_events_include_terminal_done_message() {
    let (system_event, done_event) = build_agent_hard_cap_events(200, 3, 7);

    assert_eq!(system_event["type"], "system");
    assert_eq!(
        system_event["content"],
        "Detected abnormal tool loop (200 consecutive rounds). Stopping."
    );

    assert_eq!(done_event["type"], "done");
    assert_eq!(done_event["phase"], "hard_cap");
    assert_eq!(done_event["reason"], "hard_cap");
    assert_eq!(done_event["cycles"], 3);
    assert_eq!(done_event["tool_calls"], 7);
}

#[test]
fn update_llm_response_usage_uses_request_estimate_when_provider_usage_missing() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = std::sync::Arc::new(test_app_state());
    let session = test_session("usage-session", "Usage Session", None);
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let cancel = CancellationToken::new();
    let run_cancel = cancel.child_token();

    rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .insert(session.id.clone(), session.clone());

        let ctx = AgentRunCtx {
            state: &state,
            config: state.config(),
            model: state.config().model.clone(),
            current_session_id: "usage-session",
            cancel: &cancel,
            live_tx: &live_tx,
            run_cancel: &run_cancel,
        };
        let resp = providers::LlmResponse {
            message: ChatMessage {
                role: "assistant".into(),
                content: Some("done".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            input_tokens: None,
            output_tokens: None,
            tool_image_compatibility_fallback: false,
        };

        update_llm_response_usage(
            &ctx,
            Provider::OpenAI,
            "openai",
            crate::context::USAGE_ROLE_PRIMARY,
            777,
            &resp,
        )
        .await;
    });

    let persisted = rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .get("usage-session")
            .cloned()
            .expect("session should exist")
    });

    assert_eq!(persisted.input_tokens, 777);
    assert_eq!(persisted.input_token_source, "estimated");
    assert_eq!(
        persisted.daily_provider_usage[&crate::context::usage_provider_label("openai")][0],
        777
    );
    assert_eq!(
        persisted.daily_provider_usage[&crate::context::usage_provider_label("openai")][1],
        persisted.output_tokens
    );
    assert_eq!(
        persisted.daily_provider_usage
            [&crate::context::usage_role_label(crate::context::USAGE_ROLE_PRIMARY)],
        [777, persisted.output_tokens]
    );
}

#[test]
fn update_session_token_usage_with_providers_merges_breakdown() {
    let mut session = test_session("usage-session", "Usage Session", None);
    let mut provider_usage = HashMap::new();
    provider_usage.insert(crate::context::usage_provider_label("openai"), [100, 25]);
    provider_usage.insert(crate::context::usage_provider_label("anthropic"), [50, 10]);
    provider_usage.insert(
        crate::context::usage_role_label(crate::context::USAGE_ROLE_SUB_AGENT),
        [150, 35],
    );

    crate::update_session_token_usage_with_providers(
        &mut session,
        150,
        35,
        "estimated",
        "estimated",
        &provider_usage,
    );

    assert_eq!(session.input_tokens, 150);
    assert_eq!(session.output_tokens, 35);
    assert_eq!(
        session.daily_provider_usage[&crate::context::usage_provider_label("openai")],
        [100, 25]
    );
    assert_eq!(
        session.daily_provider_usage[&crate::context::usage_provider_label("anthropic")],
        [50, 10]
    );
    assert_eq!(
        session.daily_provider_usage
            [&crate::context::usage_role_label(crate::context::USAGE_ROLE_SUB_AGENT)],
        [150, 35]
    );
}

#[test]
fn rollover_daily_usage_caps_usage_history_at_limit() {
    let mut session = test_session("cap-session", "Cap Session", None);
    // Pre-fill exactly USAGE_HISTORY_CAP snapshots.
    for i in 0..crate::USAGE_HISTORY_CAP {
        session.usage_history.push(crate::DailyUsageSnapshot {
            date: format!("2025-01-{:02}", i + 1),
            input: 10,
            output: 5,
            ..Default::default()
        });
    }
    assert_eq!(session.usage_history.len(), crate::USAGE_HISTORY_CAP);

    // Set up a stale day so that rollover will push a new snapshot.
    session.token_usage_day = "2025-02-01".to_string();
    session.daily_input_tokens = 42;
    session.daily_output_tokens = 7;

    crate::context::rollover_daily_usage_if_needed(&mut session);

    // History should still be capped at the limit.
    assert_eq!(session.usage_history.len(), crate::USAGE_HISTORY_CAP);
    // The oldest entry (2025-01-01) should have been evicted.
    assert_eq!(session.usage_history[0].date, "2025-01-02");
    // The newest entry should be the one just rolled over.
    let last = session.usage_history.last().expect("should have entries");
    assert_eq!(last.date, "2025-02-01");
    assert_eq!(last.input, 42);
    assert_eq!(last.output, 7);
}

#[test]
fn update_llm_response_usage_uses_configured_provider_name() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let mut providers = HashMap::new();
    providers.insert(
        "openai-work".to_string(),
        JsonProviderConfig {
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "key-work".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "gpt-4o-mini".to_string(),
                name: None,
                reasoning: Some(false),
                effort: None,
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(16384),
                compat: None,
            }],
        },
    );
    let config = Config {
        model: "openai-work/gpt-4o-mini".to_string(),
        api_base: "https://gateway.example/v1".to_string(),
        api_key: "key-work".to_string(),
        providers,
        ..test_config()
    };
    let state = std::sync::Arc::new(test_app_state_with_config(config.clone()));
    let session = test_session("usage-session", "Usage Session", Some(&config.model));
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) =
        mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
    let cancel = CancellationToken::new();
    let run_cancel = cancel.child_token();

    rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .insert(session.id.clone(), session.clone());

        let ctx = AgentRunCtx {
            state: &state,
            config: state.config(),
            model: state.config().model.clone(),
            current_session_id: "usage-session",
            cancel: &cancel,
            live_tx: &live_tx,
            run_cancel: &run_cancel,
        };
        let resp = providers::LlmResponse {
            message: ChatMessage {
                role: "assistant".into(),
                content: Some("done".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            input_tokens: Some(321),
            output_tokens: Some(12),
            tool_image_compatibility_fallback: false,
        };

        let config = state.config();
        let provider_name = config.resolve_provider_name(&config.model);
        update_llm_response_usage(
            &ctx,
            Provider::OpenAI,
            &provider_name,
            crate::context::USAGE_ROLE_PRIMARY,
            321,
            &resp,
        )
        .await;
    });

    let persisted = rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .get("usage-session")
            .cloned()
            .expect("session should exist")
    });

    assert_eq!(
        persisted.daily_provider_usage[&crate::context::usage_provider_label("openai-work")],
        [321, 12]
    );
}

#[test]
fn resolve_input_image_url_prefers_verified_s3_object_url() {
    let s3_cfg = test_s3_config();
    let object_key = "images/2026/demo.png";
    let token = crate::image_uploads::sign_attachment_object_key(&s3_cfg, object_key);
    let config_id = crate::image_uploads::s3_config_id(&s3_cfg);

    let (url, trusted_object_key) = socket_input::resolve_input_image_url(
        "https://example.com/decoy.png",
        Some(object_key),
        Some(&token),
        Some(&config_id),
        Some(&s3_cfg),
    )
    .expect("verified uploads should resolve to a trusted S3 URL");

    assert_eq!(trusted_object_key.as_deref(), Some(object_key));
    assert!(url.starts_with("https://minio.example.test/storage/bucket/images/2026/demo.png?"));
    assert!(url.contains("X-Amz-Signature="));
}

#[test]
fn resolve_input_image_url_rejects_incomplete_uploaded_metadata() {
    let err = socket_input::resolve_input_image_url(
        "https://example.com/photo.png",
        Some("images/2026/demo.png"),
        None,
        None,
        Some(&test_s3_config()),
    )
    .expect_err("partial upload metadata should be rejected");

    assert_eq!(
        err,
        "Incomplete uploaded image metadata. Please re-attach the image."
    );
}

#[test]
fn resolve_input_image_url_rejects_upload_from_previous_s3_configuration() {
    let original_cfg = test_s3_config();
    let object_key = "images/2026/demo.png";
    let token = crate::image_uploads::sign_attachment_object_key(&original_cfg, object_key);
    let original_config_id = crate::image_uploads::s3_config_id(&original_cfg);
    let mut current_cfg = original_cfg.clone();
    current_cfg.endpoint = "https://replacement.example.test/storage".to_string();

    let err = socket_input::resolve_input_image_url(
        "https://example.com/decoy.png",
        Some(object_key),
        Some(&token),
        Some(&original_config_id),
        Some(&current_cfg),
    )
    .expect_err("uploads must stay bound to the S3 configuration that accepted them");

    assert_eq!(
        err,
        "S3 upload configuration changed. Please re-attach the image."
    );
}

#[test]
fn try_claim_reflection_requires_minimum_cycles() {
    let _guard = reflection_test_guard().blocking_lock();
    // Reset cooldown so it doesn't interfere.
    LAST_REFLECTION_EPOCH.store(0, std::sync::atomic::Ordering::Relaxed);

    assert!(try_claim_reflection(0, 5).is_none());
    assert!(try_claim_reflection(1, 10).is_none());
    assert!(try_claim_reflection(2, 20).is_none());

    // cycles >= 3 should succeed and return the previous epoch.
    let prev = try_claim_reflection(3, 1);
    assert!(prev.is_some());
    let (prev_epoch, claimed_epoch) = prev.unwrap();
    rollback_reflection_claim(prev_epoch, claimed_epoch); // restore for next assertion

    let prev = try_claim_reflection(10, 5);
    assert!(prev.is_some());
    let (prev_epoch, claimed_epoch) = prev.unwrap();
    rollback_reflection_claim(prev_epoch, claimed_epoch);
}

#[test]
fn reflection_runtime_generation_invalidates_stale_tasks_after_disable() {
    let _guard = reflection_test_guard().blocking_lock();

    refresh_reflection_runtime(true);
    let generation = reflection_runtime_generation();
    assert!(reflection_runtime_enabled());
    assert!(reflection_runtime_matches(generation));

    let stable_generation = refresh_reflection_runtime(true);
    assert_eq!(stable_generation, generation);
    assert!(reflection_runtime_matches(stable_generation));

    refresh_reflection_runtime(false);
    assert!(!reflection_runtime_enabled());
    assert!(!reflection_runtime_matches(generation));

    refresh_reflection_runtime(true);
    let new_generation = reflection_runtime_generation();
    assert!(reflection_runtime_enabled());
    assert!(reflection_runtime_matches(new_generation));
}

#[test]
fn reflection_run_policy_uses_the_originating_enabled_setting() {
    let _guard = reflection_test_guard().blocking_lock();

    refresh_reflection_runtime(false);
    let disabled_config = test_config();
    refresh_reflection_runtime(true);
    assert!(
        !reflection_run_snapshot_is_enabled(&disabled_config),
        "enabling reflection must not make an older disabled run reflect"
    );

    let mut enabled_config = test_config();
    enabled_config.daily_reflection = true;
    assert!(reflection_run_snapshot_is_enabled(&enabled_config));
    refresh_reflection_runtime(false);
    assert!(
        !reflection_run_snapshot_is_enabled(&enabled_config),
        "disabling reflection must invalidate an already-running snapshot"
    );
}

#[tokio::test]
async fn stale_reflection_generation_returns_before_work_or_write() {
    let stale_generation = {
        let _guard = reflection_test_guard().lock().await;
        refresh_reflection_runtime(true);
        let stale_generation = reflection_runtime_generation();
        refresh_reflection_runtime(false);
        stale_generation
    };

    let workspace =
        std::env::temp_dir().join(format!("lingclaw-reflection-stale-{}", crate::now_epoch()));
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let registry = crate::auxiliary_tasks::AuxiliaryTaskRegistry::new(true, true);
    let permit = registry
        .permit(
            "main",
            crate::auxiliary_tasks::AuxiliaryTaskKind::Reflection,
        )
        .expect("reflection permit should be available");
    let task = registry
        .spawn(permit, {
            let workspace = workspace.clone();
            move |task_context| async move {
                run_post_execution_reflection(PostExecutionReflectionInput {
                    config: Arc::new(test_config()),
                    http: reqwest::Client::new(),
                    sessions: Arc::new(Mutex::new(HashMap::new())),
                    session_id: "main".to_string(),
                    workspace,
                    model: "gpt-4o-mini".to_string(),
                    messages: vec![ChatMessage {
                        role: "user".into(),
                        content: Some("hello".into()),
                        images: None,
                        thinking: None,
                        anthropic_thinking_blocks: None,
                        tool_calls: None,
                        tool_call_id: None,
                        timestamp: None,
                    }],
                    policy_generation: stale_generation,
                    cycles: 3,
                    tool_calls: 1,
                    provider_timeout: Duration::from_secs(1),
                    task_context,
                    provider_succeeded_gate: None,
                })
                .await
            }
        })
        .expect("reflection task should be supervised");
    let outcome = task
        .wait()
        .await
        .expect("reflection supervisor should finish")
        .expect("stale reflection should short-circuit successfully");

    assert!(!outcome);
    let today = prompts::current_local_snapshot().today();
    assert!(
        !workspace
            .join("memory")
            .join(format!("{today}.md"))
            .exists()
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[test]
fn try_claim_reflection_respects_cooldown() {
    let _guard = reflection_test_guard().blocking_lock();
    let now = epoch_secs_now();

    // Last reflection was just now --?should be blocked.
    LAST_REFLECTION_EPOCH.store(now, std::sync::atomic::Ordering::Relaxed);
    assert!(try_claim_reflection(5, 5).is_none());

    // Last reflection was long ago --?should be allowed.
    LAST_REFLECTION_EPOCH.store(
        now - REFLECTION_COOLDOWN_SECS - 1,
        std::sync::atomic::Ordering::Relaxed,
    );
    let prev = try_claim_reflection(5, 5);
    assert!(prev.is_some());
    let (prev_epoch, claimed_epoch) = prev.unwrap();
    rollback_reflection_claim(prev_epoch, claimed_epoch);

    // Exactly at the boundary --?should be allowed.
    LAST_REFLECTION_EPOCH.store(
        now - REFLECTION_COOLDOWN_SECS,
        std::sync::atomic::Ordering::Relaxed,
    );
    let prev = try_claim_reflection(5, 5);
    assert!(prev.is_some());
    let (prev_epoch, claimed_epoch) = prev.unwrap();
    rollback_reflection_claim(prev_epoch, claimed_epoch);
}

#[test]
fn try_claim_reflection_prevents_concurrent_claims() {
    let _guard = reflection_test_guard().blocking_lock();
    // Ensure cooldown is clear.
    LAST_REFLECTION_EPOCH.store(0, std::sync::atomic::Ordering::Relaxed);

    // First claim succeeds.
    let first = try_claim_reflection(5, 5);
    assert!(first.is_some());

    // Second claim sees the just-written timestamp and fails (CAS mismatch).
    assert!(try_claim_reflection(5, 5).is_none());

    // Clean up.
    let (prev_epoch, claimed_epoch) = first.unwrap();
    rollback_reflection_claim(prev_epoch, claimed_epoch);
}

#[test]
fn rollback_reflection_claim_is_noop_when_slot_already_reclaimed() {
    let _guard = reflection_test_guard().blocking_lock();
    // Clear cooldown.
    LAST_REFLECTION_EPOCH.store(0, std::sync::atomic::Ordering::Relaxed);

    // First run claims the slot.
    let first = try_claim_reflection(5, 5);
    assert!(first.is_some());
    let (prev_epoch, claimed_epoch) = first.unwrap();

    // Simulate another run claiming a newer slot while the first reflection
    // is still in-flight (e.g. after toolTimeout > cooldown).
    let newer_epoch = claimed_epoch + REFLECTION_COOLDOWN_SECS + 1;
    LAST_REFLECTION_EPOCH.store(newer_epoch, std::sync::atomic::Ordering::Relaxed);

    // The first run's rollback should be a no-op --?CAS fails because the
    // stored value (newer_epoch) != claimed_epoch.
    rollback_reflection_claim(prev_epoch, claimed_epoch);
    assert_eq!(
        LAST_REFLECTION_EPOCH.load(std::sync::atomic::Ordering::Relaxed),
        newer_epoch,
        "rollback must not overwrite a newer legitimate claim"
    );
}

#[test]
fn reflection_model_or_fallback_chain() {
    // No reflection_model, no memory_model --?use fallback.
    let mut config = test_config();
    assert_eq!(config.reflection_model_or("primary-model"), "primary-model");

    // memory_model set --?reflection inherits from memory.
    config.memory_model = Some("memory-llm".to_string());
    assert_eq!(config.reflection_model_or("primary-model"), "memory-llm");

    // reflection_model set --?overrides memory_model.
    config.reflection_model = Some("reflection-llm".to_string());
    assert_eq!(
        config.reflection_model_or("primary-model"),
        "reflection-llm"
    );
}

#[tokio::test]
async fn round21_hard_cap_settles_planning_and_executing_plan() {
    for status in [
        crate::plan::PlanStatus::Planning,
        crate::plan::PlanStatus::Executing,
    ] {
        let state = Arc::new(test_app_state());
        let id = format!("hard-cap-{}", crate::generate_random_session_id().unwrap());
        let mut session = test_session(&id, "Hard cap", None);
        session.messages.push(ChatMessage {
            role: "user".into(),
            content: Some("hard cap request".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(21),
        });
        let mut plan = completion_verifier_test_plan("hard-cap-plan");
        plan.status = status;
        session.pending_plan = Some(plan.clone());
        state.sessions.lock().await.insert(id.clone(), session);
        let cancel = CancellationToken::new();
        let run_cancel = CancellationToken::new();
        let (live_tx, mut live_rx) = mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
        let ctx = AgentRunCtx {
            state: &state,
            config: state.config(),
            model: state.config().model.clone(),
            current_session_id: &id,
            cancel: &cancel,
            live_tx: &live_tx,
            run_cancel: &run_cancel,
        };
        let mut phase = phase_state_for_analyze_test();
        phase.round = AGENT_HARD_CAP_ROUNDS;
        phase.terminal_plan_identity = Some((plan.id.clone(), plan.revision));
        if status == crate::plan::PlanStatus::Planning {
            phase.run_mode = AgentRunMode::PlanOnly;
        } else {
            phase.approved_plan = Some(plan.clone());
        }
        assert!(matches!(
            run_analyze_phase(&ctx, &mut phase).await,
            AgentPhaseControl::Break
        ));
        let final_plan = state.sessions.lock().await[&id]
            .pending_plan
            .clone()
            .unwrap();
        assert_eq!(final_plan.status, crate::plan::PlanStatus::Failed);
        assert_eq!(final_plan.progress.len(), plan.progress.len());
        let mut events = Vec::new();
        while let Ok(event) = live_rx.try_recv() {
            events.push(event);
        }
        assert_eq!(events.iter().filter(|e| e["type"] == "done").count(), 1);
        assert!(
            events
                .iter()
                .any(|e| e["type"] == "plan_state" && e["plan"]["status"] == "failed")
        );
        let _ =
            std::fs::remove_file(crate::session_store::sessions_dir().join(format!("{id}.json")));
    }
}

#[tokio::test]
async fn round21_hard_cap_generation_failure_and_precommit_stop_never_publish_a_failed_plan() {
    for scenario in ["generation", "storage", "stop"] {
        let state = Arc::new(test_app_state());
        let id = format!(
            "cap-{scenario}-{}",
            crate::generate_random_session_id().unwrap()
        );
        let mut session = test_session(&id, "Hard cap isolation", None);
        session.messages.push(ChatMessage {
            role: "user".into(),
            content: Some("cap isolation".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(21),
        });
        let mut plan = completion_verifier_test_plan("cap-isolation-plan");
        if scenario == "generation" {
            plan.revision += 1;
        }
        session.pending_plan = Some(plan.clone());
        state.sessions.lock().await.insert(id.clone(), session);
        let failure_path = crate::session_store::sessions_dir().join(format!("{id}.json.tmp"));
        if scenario == "storage" {
            std::fs::create_dir_all(&failure_path).unwrap();
        }
        let cancel = CancellationToken::new();
        let run_cancel = CancellationToken::new();
        let stop_requested = Arc::new(AtomicBool::new(false));
        state.active_runs.lock().await.insert(
            id.clone(),
            SessionRunBinding {
                connection_id: 21,
                cancel: run_cancel.clone(),
                stop_requested: stop_requested.clone(),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
        let (live_tx, mut live_rx) = mpsc::channel(LIVE_EVENT_CHANNEL_CAPACITY);
        let ctx = AgentRunCtx {
            state: &state,
            config: state.config(),
            model: state.config().model.clone(),
            current_session_id: &id,
            cancel: &cancel,
            live_tx: &live_tx,
            run_cancel: &run_cancel,
        };
        let mut phase = phase_state_for_analyze_test();
        phase.round = AGENT_HARD_CAP_ROUNDS;
        phase.terminal_plan_identity = Some((
            plan.id.clone(),
            if scenario == "generation" {
                plan.revision - 1
            } else {
                plan.revision
            },
        ));
        phase.approved_plan = Some(plan.clone());
        let gate = Arc::new(FinishPhaseTestGate::new(
            FinishPhaseTestStage::PreTerminalCommit,
        ));
        if scenario == "stop" {
            phase.completion_evidence.finish_gate = Some(gate.clone());
            let trigger = async {
                wait_for_finish_phase_gate(&gate).await;
                stop_requested.store(true, Ordering::Relaxed);
                run_cancel.cancel();
            };
            tokio::join!(run_analyze_phase(&ctx, &mut phase), trigger);
        } else {
            run_analyze_phase(&ctx, &mut phase).await;
        }
        let actual = state.sessions.lock().await[&id]
            .pending_plan
            .clone()
            .unwrap();
        assert_eq!(actual.status, plan.status, "{scenario}");
        assert_eq!(actual.revision, plan.revision);
        let events = std::iter::from_fn(|| live_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            events
                .iter()
                .all(|e| e["type"] != "done" && e["type"] != "plan_state"),
            "{scenario}: {events:?}"
        );
        if scenario == "generation" {
            assert_eq!(
                events
                    .iter()
                    .filter(|e| e["code"] == "terminal_identity_unavailable")
                    .count(),
                1
            );
        }
        if scenario == "stop" {
            assert!(phase.run_stopped);
            assert_eq!(
                phase.completion_evidence.terminal_linearization,
                TerminalLinearizationState::PreCommit
            );
        }
        if scenario == "storage" {
            std::fs::remove_dir_all(&failure_path).unwrap();
        }
        let _ =
            std::fs::remove_file(crate::session_store::sessions_dir().join(format!("{id}.json")));
    }
}
