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
    }
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

    assert!(matches!(
        action,
        IdleSocketInputAction::StartAgent {
            run_mode: AgentRunMode::Execute,
            ..
        }
    ));
    let sessions = state.sessions.lock().await;
    let session = sessions.get(&session_id).expect("session should exist");
    assert!(session.pending_plan.is_none());
    assert_eq!(
        session
            .messages
            .last()
            .and_then(|message| message.content.as_deref()),
        Some("Proceed with the approved plan.")
    );
    let _ = std::fs::remove_file(
        crate::session_store::sessions_dir().join(format!("{session_id}.json")),
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
    assert_eq!(parsed["type"].as_str(), Some("system"));
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

    let current_events = [
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
    ];
    let current_types = current_events
        .iter()
        .map(|payload| payload["type"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(current_types.contains(&"session".to_string()));
    assert!(current_types.contains(&"view_state".to_string()));
    assert!(current_types.contains(&"todos_state".to_string()));
    assert!(current_types.contains(&"history".to_string()));
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
    ];
    let current_types = current_events
        .iter()
        .map(|payload| payload["type"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(current_types.contains(&"session".to_string()));
    assert!(current_types.contains(&"view_state".to_string()));
    assert!(current_types.contains(&"todos_state".to_string()));
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
    ];
    let current_types = current_events
        .iter()
        .map(|payload| payload["type"].as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    assert!(current_types.contains(&"session".to_string()));
    assert!(current_types.contains(&"view_state".to_string()));
    assert!(current_types.contains(&"todos_state".to_string()));
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

    switch_socket_session(
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

    if result.session_list_changed {
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
    let state = Arc::new(test_app_state());
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

    assert!(!outcome.rerun_agent);
    assert!(!outcome.shutting_down);

    let done_event = live_rx.recv().await.expect("done event should be emitted");
    assert_eq!(done_event["type"].as_str(), Some("done"));
    assert_eq!(done_event["phase"].as_str(), Some("stopped"));
    assert_eq!(done_event["reason"].as_str(), Some("user_stop"));
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
    assert_eq!(persisted.messages.len(), 2);
    assert_eq!(persisted.messages[0].role, "system");
    assert_eq!(persisted.messages[1].role, "user");
    assert_eq!(
        persisted.messages[1].content.as_deref(),
        Some("follow-up detail")
    );
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
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        pending_plan: None,
        version: 0,
        workspace: PathBuf::new(),
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
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(true),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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

    let tools = build_plan_only_tools(&config, Provider::OpenAI, &workspace);
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
        .into_iter()
        .filter_map(|tool| tool_definition_name(&tool).map(str::to_string))
        .collect::<Vec<_>>();

    assert!(main_tools.contains(&crate::tools::TOOL_NAME_SESSION_CONTROL.to_string()));
    assert!(!other_tools.contains(&crate::tools::TOOL_NAME_SESSION_CONTROL.to_string()));
    assert!(!plan_tools.contains(&crate::tools::TOOL_NAME_SESSION_CONTROL.to_string()));

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
    let (outcome, effective_args) = execute_tool_call(&ctx, &mut phase_state, &tc, &image_budget)
        .await
        .expect("plan-only session_control rejection should be recorded");

    assert!(outcome.is_error);
    assert!(outcome.output.contains("rejected by plan mode"));
    assert!(effective_args.is_none());
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
        !is_plan_only_allowed_tool(exposed_name, &config, &workspace),
        "PlanOnly must reject MCP tools not enabled by the session policy"
    );

    crate::tools::mcp::clear_cached_runtime_state_for_server("mock");
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn plan_only_allowed_tool_accepts_policy_enabled_read_only_mcp_tool() {
    let _guard = crate::tools::mcp::acquire_mcp_test_guard().await;
    let config = plan_only_mcp_test_config();
    let workspace = temp_workspace("plan-only-mcp-policy-allow");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let exposed_name = "mcp__mock__search__def456";
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
            enabled_tools: HashSet::from([exposed_name.to_string()]),
            ..Default::default()
        },
    )
    .expect("policy should save");

    assert!(is_plan_only_allowed_tool(exposed_name, &config, &workspace));

    crate::tools::mcp::clear_cached_runtime_state_for_server("mock");
    let _ = std::fs::remove_dir_all(&workspace);
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
    assert!(prompt.contains("## Plan Mode"));
    assert!(prompt.contains("Do not modify files"));
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
    let (outcome, effective_args) = execute_tool_call(&ctx, &mut phase_state, &tc, &image_budget)
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
async fn run_finish_phase_plan_only_registers_plan_without_memory_enqueue() {
    let mut config = test_config();
    config.structured_memory = true;
    let state = Arc::new(test_app_state_with_config(config.clone()));
    let queue = crate::memory::MemoryUpdateQueue::spawn(config, state.sessions.clone());
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
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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
        pending_interventions: Vec::new(),
        react_ctx: agent::AgentLoopCtx::new(false),
        shutting_down: false,
        run_stopped: false,
        run_failed: false,
        run_detached: false,
        last_save_instant: None,
        usage_snap_input: 0,
        usage_snap_output: 0,
        tool_images_disabled: false,
        tool_images_attached_in_batch: 0,
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

    let control = record_tool_result(&ctx, &mut phase_state, &tool_call, result, None).await;

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
    });

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
    assert!(persisted.pending_plan.is_none());
    assert!(persisted.updated_at >= 1);

    let saved = crate::session_store::load_session_from_disk("session-a")
        .expect("session should be saved to disk");
    assert!(saved.pending_plan.is_none());
    assert_eq!(saved.messages.len(), 3);
    let _ = std::fs::remove_file(crate::session_store::sessions_dir().join("session-a.json"));
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
