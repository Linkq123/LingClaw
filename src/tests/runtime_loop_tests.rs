use super::*;

use std::{collections::HashMap, sync::atomic::AtomicU64};

use crate::config::{JsonModelEntry, JsonProviderConfig, S3Config};

fn test_config() -> Config {
    Config {
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
    }
}

fn test_app_state() -> AppState {
    AppState {
        config: std::sync::Mutex::new(Arc::new(test_config())),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    }
}

#[test]
fn effective_think_level_auto_treats_gemini3_as_reasoning_capable() {
    let resolved = providers::ResolvedModel {
        provider: Provider::Gemini,
        api_base: Provider::Gemini.default_api_base().into(),
        api_key: "gemini-key".into(),
        model_id: "gemini-3-flash-preview".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(512),
        context_window: 1_000_000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };

    assert_eq!(
        effective_think_level("auto", &resolved, 0, false, 10, 0),
        "medium"
    );
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
    let (live_tx, _live_rx) = tokio::sync::mpsc::channel::<serde_json::Value>(4);

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
async fn handle_idle_socket_input_queues_new_prompt_while_reconnected_run_active() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
    let (live_tx, _live_rx) = tokio::sync::mpsc::channel::<serde_json::Value>(4);
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
async fn handle_idle_socket_input_rejects_intervention_when_reconnected_run_is_finishing() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let mut current_session_id = session_id.clone();
    let current_session_ref = Arc::new(Mutex::new(session_id.clone()));
    let cancel = CancellationToken::new();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
    let (live_tx, _live_rx) = tokio::sync::mpsc::channel::<serde_json::Value>(4);
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
    let (live_tx, mut live_rx) = tokio::sync::mpsc::channel::<serde_json::Value>(4);
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
async fn apply_run_cancel_outcome_treats_shared_stop_as_user_stop() {
    let state = Arc::new(test_app_state());
    let session_id = MAIN_SESSION_ID.to_string();
    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let stop_requested = Arc::new(AtomicBool::new(true));
    let deferred_interventions = Arc::new(Mutex::new(DeferredInterventionState::open()));
    let (live_tx, _live_rx) = tokio::sync::mpsc::channel::<serde_json::Value>(4);

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
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        cycle_workspace: PathBuf::new(),
        last_observation_hint: None,
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
    };

    apply_run_cancel_outcome(&ctx, &mut phase_state).await;

    assert!(phase_state.run_stopped);
    assert!(!phase_state.run_detached);
    assert!(!stop_requested.load(std::sync::atomic::Ordering::Relaxed));
}

fn test_app_state_with_config(config: Config) -> AppState {
    AppState {
        config: std::sync::Mutex::new(Arc::new(config)),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    }
}

fn test_session(id: &str, name: &str, model_override: Option<&str>) -> Session {
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
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        version: 0,
        workspace: PathBuf::new(),
    }
}

#[tokio::test]
async fn apply_llm_response_persists_multi_tool_assistant_with_thinking() {
    let state = Arc::new(test_app_state());
    let session_id = "deepseek-session".to_string();
    let cancel = CancellationToken::new();
    let run_cancel = CancellationToken::new();
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) = mpsc::channel(8);

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            session_id.clone(),
            test_session(&session_id, "DeepSeek", None),
        );
    }

    let ctx = AgentRunCtx {
        state: &state,
        current_session_id: &session_id,
        cancel: &cancel,
        live_tx: &live_tx,
        run_cancel: &run_cancel,
    };
    let mut phase_state = AgentPhaseState {
        round: 0,
        pending_tool_calls: Vec::new(),
        collected_results: Vec::new(),
        cycle_workspace: PathBuf::new(),
        last_observation_hint: None,
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
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
    };

    apply_llm_response(
        &ctx,
        &mut phase_state,
        Provider::OpenAI,
        "deepseek".to_string(),
        crate::context::USAGE_ROLE_PRIMARY,
        123,
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
        s3_object_key: None,
        cache_path: None,
        data: None,
    }
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
        Some("杩欏紶鍥鹃噷鏄粈涔堬紵"),
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
    let (inbound_tx, mut inbound_rx) = mpsc::channel(8);
    let (live_tx, mut live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) = mpsc::channel(8);
    let run_cancel = CancellationToken::new();
    let mut pending = Vec::new();

    rt.block_on(async {
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

        let stopped =
            drain_busy_socket_messages(&mut inbound_rx, &mut pending, &live_tx, &run_cancel).await;

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
fn persist_pending_interventions_appends_user_messages() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = std::sync::Arc::new(test_app_state());
    let mut session = test_session("session-a", "Session A", None);
    session.updated_at = 1;

    rt.block_on(async {
        state
            .sessions
            .lock()
            .await
            .insert(session.id.clone(), session.clone());
    });

    let mut pending = vec!["first note".to_string(), "second note".to_string()];
    rt.block_on(persist_pending_interventions(
        &state,
        "session-a",
        &mut pending,
    ));

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
    assert!(persisted.updated_at >= 1);
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
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) = mpsc::channel(4);
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
    let (live_tx, _live_rx): (LiveTx, mpsc::Receiver<serde_json::Value>) = mpsc::channel(4);
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

    let (url, trusted_object_key) = socket_input::resolve_input_image_url(
        "https://example.com/decoy.png",
        Some(object_key),
        Some(&token),
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
        Some(&test_s3_config()),
    )
    .expect_err("partial upload metadata should be rejected");

    assert_eq!(
        err,
        "Incomplete uploaded image metadata. Please re-attach the image."
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
fn cancel_active_reflections_cancels_registered_tasks() {
    let _guard = reflection_test_guard().blocking_lock();
    cancel_active_reflections();

    let cancel = CancellationToken::new();
    let _task_id = register_active_reflection(cancel.clone());
    assert!(!cancel.is_cancelled());

    cancel_active_reflections();

    assert!(cancel.is_cancelled());
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

    let outcome = run_post_execution_reflection(PostExecutionReflectionInput {
        config: Arc::new(test_config()),
        http: reqwest::Client::new(),
        sessions: Arc::new(Mutex::new(HashMap::new())),
        session_id: "main".to_string(),
        workspace: workspace.clone(),
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
    })
    .await
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

    // Last reflection was just now 鈥?should be blocked.
    LAST_REFLECTION_EPOCH.store(now, std::sync::atomic::Ordering::Relaxed);
    assert!(try_claim_reflection(5, 5).is_none());

    // Last reflection was long ago 鈥?should be allowed.
    LAST_REFLECTION_EPOCH.store(
        now - REFLECTION_COOLDOWN_SECS - 1,
        std::sync::atomic::Ordering::Relaxed,
    );
    let prev = try_claim_reflection(5, 5);
    assert!(prev.is_some());
    let (prev_epoch, claimed_epoch) = prev.unwrap();
    rollback_reflection_claim(prev_epoch, claimed_epoch);

    // Exactly at the boundary 鈥?should be allowed.
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

    // The first run's rollback should be a no-op 鈥?CAS fails because the
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
    // No reflection_model, no memory_model 鈫?use fallback.
    let mut config = test_config();
    assert_eq!(config.reflection_model_or("primary-model"), "primary-model");

    // memory_model set 鈫?reflection inherits from memory.
    config.memory_model = Some("memory-llm".to_string());
    assert_eq!(config.reflection_model_or("primary-model"), "memory-llm");

    // reflection_model set 鈫?overrides memory_model.
    config.reflection_model = Some("reflection-llm".to_string());
    assert_eq!(
        config.reflection_model_or("primary-model"),
        "reflection-llm"
    );
}
