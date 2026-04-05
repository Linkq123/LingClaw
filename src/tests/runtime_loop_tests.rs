use super::*;

use std::{collections::HashMap, sync::atomic::AtomicU64};

fn test_config() -> Config {
    Config {
        api_key: "env-key".to_string(),
        api_base: "https://fallback.example/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        fast_model: None,
        sub_agent_model: None,
        memory_model: None,
        provider: Provider::OpenAI,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: DEFAULT_PORT,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        tool_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
        openai_stream_include_usage: false,
        structured_memory: false,
    }
}

fn test_app_state() -> AppState {
    AppState {
        config: test_config(),
        http: reqwest::Client::new(),
        sessions: Mutex::new(HashMap::new()),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        active_runs: Mutex::new(HashMap::new()),
        connection_cancels: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        hooks: HookRegistry::new(),
        memory_queue: None,
    }
}

fn test_session(id: &str, name: &str, model_override: Option<&str>) -> Session {
    Session {
        id: id.to_string(),
        name: name.to_string(),
        messages: vec![ChatMessage {
            role: "system".into(),
            content: Some("system".into()),
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
        model_override: model_override.map(|value| value.to_string()),
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        version: 0,
        workspace: PathBuf::new(),
    }
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
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            input_tokens: None,
            output_tokens: None,
        };

        update_llm_response_usage(&ctx, Provider::OpenAI, 777, &resp).await;
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
}
