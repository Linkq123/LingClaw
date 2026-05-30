use super::*;
use crate::config::JsonMcpServerConfig;
use crate::session_store::load_session_from_disk;
use crate::session_store::replace_session_file_from_temp;
use axum::http::{HeaderMap, HeaderValue};
use serde_json::json;
use std::{collections::HashMap, sync::atomic::AtomicU64};

/// RAII guard that cleans up a saved session's JSON file and workspace directory on drop.
/// This ensures cleanup runs even if the test panics.
struct SavedSessionGuard {
    session_id: String,
    workspace: PathBuf,
}

impl Drop for SavedSessionGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(sessions_dir().join(format!("{}.json", self.session_id)));
        if let Some(session_dir) = self.workspace.parent() {
            let _ = std::fs::remove_dir_all(session_dir);
        }
    }
}

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
        enable_state_digest: true,
    }
}

#[test]
fn default_port_constant_is_18989() {
    assert_eq!(DEFAULT_PORT, 18989);
}

#[test]
fn normalized_s3_prefix_defaults_when_empty() {
    assert_eq!(
        crate::config::normalized_s3_prefix(Some("  /  ".to_string())),
        "lingclaw/images/"
    );
}

#[test]
fn normalized_s3_region_defaults_and_lowercases() {
    assert_eq!(crate::config::normalized_s3_region("  "), "us-east-1");
    assert_eq!(
        crate::config::normalized_s3_region(" CN-NORTH-1 "),
        "cn-north-1"
    );
}

#[test]
fn normalized_s3_prefix_trims_and_enforces_trailing_slash() {
    assert_eq!(
        crate::config::normalized_s3_prefix(Some(" /tmp/uploads// ".to_string())),
        "tmp/uploads/"
    );
}

#[test]
fn normalized_s3_endpoint_defaults_to_regional_aws_host() {
    assert_eq!(
        crate::config::normalized_s3_endpoint(None, "eu-west-1"),
        "https://s3.eu-west-1.amazonaws.com"
    );
}

#[test]
fn normalized_s3_endpoint_rewrites_legacy_aws_global_host() {
    assert_eq!(
        crate::config::normalized_s3_endpoint(
            Some("https://s3.amazonaws.com".to_string()),
            "ap-southeast-2",
        ),
        "https://s3.ap-southeast-2.amazonaws.com"
    );
}

#[test]
fn normalized_s3_endpoint_defaults_to_aws_china_host() {
    assert_eq!(
        crate::config::normalized_s3_endpoint(None, "cn-north-1"),
        "https://s3.cn-north-1.amazonaws.com.cn"
    );
}

#[test]
fn normalized_s3_endpoint_defaults_to_aws_china_host_for_mixed_case_region() {
    assert_eq!(
        crate::config::normalized_s3_endpoint(None, " CN-NORTH-1 "),
        "https://s3.cn-north-1.amazonaws.com.cn"
    );
}

#[test]
fn normalized_s3_endpoint_rewrites_official_aws_host_for_china_region() {
    assert_eq!(
        crate::config::normalized_s3_endpoint(
            Some("https://s3.us-east-1.amazonaws.com".to_string()),
            "cn-northwest-1",
        ),
        "https://s3.cn-northwest-1.amazonaws.com.cn"
    );
}

#[test]
fn normalized_s3_endpoint_preserves_custom_gateway_paths() {
    assert_eq!(
        crate::config::normalized_s3_endpoint(
            Some("https://minio.example.test/storage/".to_string()),
            "us-east-1",
        ),
        "https://minio.example.test/storage"
    );
}

#[test]
fn memory_model_prefers_dedicated_config() {
    let config = Config {
        memory_model: Some("openai/gpt-4o-mini".to_string()),
        ..test_config()
    };

    assert_eq!(
        config.memory_model_or("openai/gpt-4o"),
        "openai/gpt-4o-mini"
    );
}

#[test]
fn memory_model_falls_back_when_unset() {
    let config = test_config();

    assert_eq!(
        config.memory_model_or("ollama/gemma4:e4b"),
        "ollama/gemma4:e4b"
    );
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

#[tokio::test]
async fn sync_memory_queue_hot_toggles_structured_memory_runtime() {
    let state = test_app_state();
    assert!(state.memory_queue().is_none());

    let mut enabled = test_config();
    enabled.structured_memory = true;
    state.sync_memory_queue(&enabled);

    let queue = state
        .memory_queue()
        .expect("structured memory should create a runtime queue");
    let status = crate::memory::memory_runtime_status(Some(&queue));
    assert!(status.contains("Memory Updater"));
    assert!(!status.contains("unavailable"));

    let mut disabled = enabled;
    disabled.structured_memory = false;
    state.sync_memory_queue(&disabled);

    assert!(state.memory_queue().is_none());
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
        version: 0,
        workspace: PathBuf::new(),
    }
}

fn make_message(role: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: Some(content.to_string()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }
}

#[test]
fn auto_compress_cutoff_preserves_recent_turns() {
    let messages = vec![
        make_message("system", "system"),
        make_message("user", "u1"),
        make_message("assistant", "a1"),
        make_message("user", "u2"),
        make_message("assistant", "a2"),
        make_message("user", "u3"),
        make_message("assistant", "a3"),
        make_message("user", "u4"),
        make_message("assistant", "a4"),
    ];

    let cutoff = find_auto_compress_cutoff(&messages, 2);

    assert_eq!(cutoff, Some(5));
}

#[test]
fn build_compressed_messages_inserts_summary_and_keeps_recent_tail() {
    let messages = vec![
        make_message("system", "system"),
        make_message("user", "old-user"),
        make_message("assistant", "old-assistant"),
        make_message("user", "recent-user"),
        make_message("assistant", "recent-assistant"),
    ];

    let compressed = build_compressed_messages(&messages, 3, "summary body");

    assert_eq!(compressed.len(), 4);
    assert_eq!(compressed[0].role, "system");
    assert_eq!(compressed[1].role, "assistant");
    assert!(
        compressed[1]
            .content
            .as_deref()
            .is_some_and(|text| text.starts_with("## Context Summary (auto-generated)"))
    );
    assert_eq!(compressed[2].content.as_deref(), Some("recent-user"));
    assert_eq!(compressed[3].content.as_deref(), Some("recent-assistant"));
}

#[test]
fn compression_source_text_skips_auto_generated_summary() {
    let summary_msg = build_auto_summary_message("Some previous summary");
    let messages = vec![
        make_message("system", "system"),
        summary_msg,
        make_message("user", "after-summary"),
        make_message("assistant", "reply"),
    ];
    let source = build_compression_source_text(&messages);
    assert!(
        !source.contains("Context Summary (auto-generated)"),
        "compression source should not include previous auto-summaries"
    );
    assert!(source.contains("after-summary"));
    assert!(source.contains("reply"));
}

#[test]
fn compression_source_text_includes_image_markers() {
    let mut user_msg = make_message("user", "look at this");
    user_msg.images = Some(vec![
        ImageAttachment {
            url: "https://example.com/a.png".to_string(),
            s3_object_key: None,
            cache_path: None,
            data: None,
        },
        ImageAttachment {
            url: "https://example.com/b.png".to_string(),
            s3_object_key: None,
            cache_path: None,
            data: None,
        },
    ]);
    let messages = vec![
        make_message("system", "system"),
        user_msg,
        make_message("assistant", "I see"),
    ];
    let source = build_compression_source_text(&messages);
    assert!(
        source.contains("2 image(s)"),
        "compression source should note image attachments"
    );
    assert!(source.contains("look at this"));
}

#[test]
fn repeated_compression_excludes_previous_summary() {
    let messages_after_first_compress = vec![
        make_message("system", "system"),
        build_auto_summary_message("summary of early conversation"),
        make_message("user", "new question"),
        make_message("assistant", "new answer"),
        make_message("user", "follow up"),
        make_message("assistant", "follow up answer"),
    ];
    let source = build_compression_source_text(&messages_after_first_compress);
    assert!(
        !source.contains("summary of early conversation"),
        "second compression should not include the first summary text"
    );
    assert!(source.contains("new question"));
    assert!(source.contains("follow up"));
}

#[test]
fn replay_live_round_replays_compression_before_assistant_delta() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-compression-order-{}", now_epoch());
    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(8);

    rt.block_on(async {
        state.live_rounds.lock().await.insert(
            session_id.clone(),
            LiveRoundState {
                connection_id: 1,
                round: 1,
                react_visible: true,
                phase: Some("analyze".into()),
                cycle: Some(2),
                effective_model: Some("openai/gpt-4o-reasoner".into()),
                effective_think: Some("high".into()),
                latest_compression: LiveCompressionState {
                    outcome: Some("compressed".to_string()),
                    reason: None,
                    messages_removed: Some(4),
                    before_estimate: Some(5_000),
                    after_estimate: Some(4_000),
                    saved_tokens: Some(1024),
                    saved_percent: Some(18),
                    pruned_messages_removed: Some(3),
                },
                latest_auto_trace: Some(agent::AutoThinkTrace {
                    round: 1,
                    cycle: 2,
                    phase: "analyze".to_string(),
                    model: "openai/gpt-4o-reasoner".to_string(),
                    provider: "openai".to_string(),
                    selected_think: "high".to_string(),
                    baseline_level: "medium".to_string(),
                    baseline_reason: "mid_loop_investigate".to_string(),
                    escalators: vec![],
                    dampeners: vec![],
                    clamps: vec![],
                    signals: agent::AutoThinkTraceSignals {
                        intent: "investigate".to_string(),
                        user_msg_chars: 96,
                        observation_strength: "medium".to_string(),
                        tool_results_count: 2,
                        tool_error_count: 1,
                        summary_count: 1,
                        summary_bytes: 4096,
                        stagnation_streak: 3,
                        error_streak: 1,
                        task_pressure: 2,
                        ready_to_finish: false,
                        action_oriented: true,
                        has_blocking_uncertainty: true,
                        progress_made: false,
                        retry_pattern: "same_tool".to_string(),
                        error_kind: "timeout".to_string(),
                        evidence_delta_quality: "no_meaningful_progress".to_string(),
                    },
                }),
                assistant_text: "hello replay".to_string(),
                ..Default::default()
            },
        );
    });

    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let compression = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("compression should arrive before timeout")
            .expect("compression should be queued")
    });
    let pruned = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("pruned event should arrive before timeout")
            .expect("pruned event should be queued")
    });
    let start = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("start should arrive before timeout")
            .expect("start should be queued")
    });
    let auto_trace = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("auto_trace should arrive before timeout")
            .expect("auto_trace should be queued")
    });
    let delta = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("delta should arrive before timeout")
            .expect("delta should be queued")
    });

    let compression: serde_json::Value =
        serde_json::from_str(&compression).expect("compression should be json");
    let pruned: serde_json::Value = serde_json::from_str(&pruned).expect("pruned should be json");
    let start: serde_json::Value = serde_json::from_str(&start).expect("start should be json");
    let auto_trace: serde_json::Value =
        serde_json::from_str(&auto_trace).expect("auto_trace should be json");
    let delta: serde_json::Value = serde_json::from_str(&delta).expect("delta should be json");

    assert_eq!(compression["type"], "context_compressed");
    assert_eq!(compression["messages_removed"], 4);
    assert_eq!(compression["before_estimate"], 5_000);
    assert_eq!(compression["after_estimate"], 4_000);
    assert_eq!(pruned["type"], "context_pruned");
    assert_eq!(pruned["messages_removed"], 3);
    assert_eq!(start["type"], "start");
    assert_eq!(auto_trace["type"], "auto_trace");
    assert_eq!(delta["type"], "delta");
    assert_eq!(delta["content"], "hello replay");
}

#[test]
fn replay_live_round_replays_prune_only_state() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-prune-only-order-{}", now_epoch());
    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(8);

    rt.block_on(async {
        state.live_rounds.lock().await.insert(
            session_id.clone(),
            LiveRoundState {
                connection_id: 1,
                round: 1,
                react_visible: true,
                phase: Some("analyze".into()),
                cycle: Some(2),
                effective_model: Some("openai/gpt-4o-reasoner".into()),
                effective_think: Some("high".into()),
                latest_compression: LiveCompressionState {
                    pruned_messages_removed: Some(3),
                    ..Default::default()
                },
                assistant_text: "hello replay".to_string(),
                ..Default::default()
            },
        );
    });

    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let pruned = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("pruned event should arrive before timeout")
            .expect("pruned event should be queued")
    });
    let start = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("start should arrive before timeout")
            .expect("start should be queued")
    });
    let delta = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("delta should arrive before timeout")
            .expect("delta should be queued")
    });

    let pruned: serde_json::Value = serde_json::from_str(&pruned).expect("pruned should be json");
    let start: serde_json::Value = serde_json::from_str(&start).expect("start should be json");
    let delta: serde_json::Value = serde_json::from_str(&delta).expect("delta should be json");

    assert_eq!(pruned["type"], "context_pruned");
    assert_eq!(pruned["messages_removed"], 3);
    assert_eq!(start["type"], "start");
    assert_eq!(delta["type"], "delta");
    assert_eq!(delta["content"], "hello replay");
}

#[test]
fn react_phase_analyze_clears_stale_compression_state_on_cycle_advance() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-react-phase-clear-{}", now_epoch());

    rt.block_on(async {
        state.live_rounds.lock().await.insert(
            session_id.clone(),
            LiveRoundState {
                connection_id: 1,
                round: 1,
                phase: Some("observe".into()),
                cycle: Some(0),
                latest_compression: LiveCompressionState {
                    outcome: Some("compressed".to_string()),
                    reason: None,
                    messages_removed: Some(4),
                    before_estimate: Some(5_000),
                    after_estimate: Some(4_000),
                    saved_tokens: Some(1_000),
                    saved_percent: Some(20),
                    pruned_messages_removed: Some(3),
                },
                ..Default::default()
            },
        );

        dispatch_live_event(
            &state,
            &session_id,
            1,
            json!({
                "type": "react_phase",
                "phase": "analyze",
                "cycle": 1
            }),
        )
        .await;
    });

    let round = rt.block_on(async {
        state
            .live_rounds
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .expect("live round should exist after react_phase")
    });

    assert_eq!(round.phase.as_deref(), Some("analyze"));
    assert_eq!(round.cycle, Some(1));
    assert_eq!(round.latest_compression.outcome, None);
    assert_eq!(round.latest_compression.reason, None);
    assert_eq!(round.latest_compression.messages_removed, None);
    assert_eq!(round.latest_compression.before_estimate, None);
    assert_eq!(round.latest_compression.after_estimate, None);
    assert_eq!(round.latest_compression.saved_tokens, None);
    assert_eq!(round.latest_compression.saved_percent, None);
    assert_eq!(round.latest_compression.pruned_messages_removed, None);
    assert!(!round.has_pending_pre_start_context_updates);
}

#[test]
fn replay_live_round_replays_only_new_cycle_prune_after_analyze_transition() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-new-cycle-prune-replay-{}", now_epoch());
    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(8);

    rt.block_on(async {
        state.live_rounds.lock().await.insert(
            session_id.clone(),
            LiveRoundState {
                connection_id: 1,
                round: 2,
                react_visible: true,
                phase: Some("analyze".into()),
                cycle: Some(1),
                effective_model: Some("openai/gpt-4o-reasoner".into()),
                effective_think: Some("high".into()),
                latest_compression: LiveCompressionState {
                    pruned_messages_removed: Some(2),
                    ..Default::default()
                },
                assistant_text: "hello replay".to_string(),
                ..Default::default()
            },
        );
    });

    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let first = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("first event should arrive before timeout")
            .expect("first event should be queued")
    });
    let second = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("second event should arrive before timeout")
            .expect("second event should be queued")
    });
    let third = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("third event should arrive before timeout")
            .expect("third event should be queued")
    });

    let first: serde_json::Value = serde_json::from_str(&first).expect("first should be json");
    let second: serde_json::Value = serde_json::from_str(&second).expect("second should be json");
    let third: serde_json::Value = serde_json::from_str(&third).expect("third should be json");

    assert_eq!(first["type"], "context_pruned");
    assert_eq!(first["messages_removed"], 2);
    assert_eq!(second["type"], "start");
    assert_eq!(third["type"], "delta");
}

#[test]
fn start_event_preserves_current_cycle_pre_start_compression_state() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-start-prestart-compression-{}", now_epoch());

    rt.block_on(async {
        state.live_rounds.lock().await.insert(
            session_id.clone(),
            LiveRoundState {
                connection_id: 1,
                latest_compression: LiveCompressionState {
                    outcome: Some("compressed".to_string()),
                    reason: None,
                    messages_removed: Some(4),
                    before_estimate: Some(5_000),
                    after_estimate: Some(4_000),
                    saved_tokens: Some(1_000),
                    saved_percent: Some(20),
                    pruned_messages_removed: Some(3),
                },
                has_pending_pre_start_context_updates: true,
                ..Default::default()
            },
        );

        dispatch_live_event(
            &state,
            &session_id,
            1,
            json!({
                "type": "start",
                "round": 2,
                "phase": "analyze",
                "cycle": 5,
                "react_visible": true,
                "model": "openai/gpt-4o-reasoner",
                "think_level": "high"
            }),
        )
        .await;
    });

    let round = rt.block_on(async {
        state
            .live_rounds
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .expect("live round should exist after start")
    });

    assert_eq!(round.round, 2);
    assert_eq!(round.cycle, Some(5));
    assert_eq!(
        round.latest_compression.outcome.as_deref(),
        Some("compressed")
    );
    assert_eq!(round.latest_compression.reason, None);
    assert_eq!(round.latest_compression.messages_removed, Some(4));
    assert_eq!(round.latest_compression.before_estimate, Some(5_000));
    assert_eq!(round.latest_compression.after_estimate, Some(4_000));
    assert_eq!(round.latest_compression.saved_tokens, Some(1_000));
    assert_eq!(round.latest_compression.saved_percent, Some(20));
    assert_eq!(round.latest_compression.pruned_messages_removed, Some(3));
    assert!(!round.has_pending_pre_start_context_updates);
}

#[test]
fn start_event_carries_forward_only_prune_state_into_next_cycle() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-start-prune-carry-{}", now_epoch());

    rt.block_on(async {
        state.live_rounds.lock().await.insert(
            session_id.clone(),
            LiveRoundState {
                connection_id: 1,
                round: 1,
                latest_compression: LiveCompressionState {
                    outcome: Some("compressed".to_string()),
                    reason: None,
                    messages_removed: Some(4),
                    before_estimate: Some(5_000),
                    after_estimate: Some(4_000),
                    saved_tokens: Some(1_000),
                    saved_percent: Some(20),
                    pruned_messages_removed: Some(3),
                },
                has_pending_pre_start_context_updates: false,
                ..Default::default()
            },
        );

        dispatch_live_event(
            &state,
            &session_id,
            1,
            json!({
                "type": "start",
                "round": 2,
                "phase": "analyze",
                "cycle": 5,
                "react_visible": true,
                "model": "openai/gpt-4o-reasoner",
                "think_level": "high"
            }),
        )
        .await;
    });

    let round = rt.block_on(async {
        state
            .live_rounds
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .expect("live round should exist after start")
    });

    assert_eq!(round.round, 2);
    assert_eq!(round.cycle, Some(5));
    assert_eq!(round.latest_compression.outcome, None);
    assert_eq!(round.latest_compression.reason, None);
    assert_eq!(round.latest_compression.messages_removed, None);
    assert_eq!(round.latest_compression.before_estimate, None);
    assert_eq!(round.latest_compression.after_estimate, None);
    assert_eq!(round.latest_compression.saved_tokens, None);
    assert_eq!(round.latest_compression.saved_percent, None);
    assert_eq!(round.latest_compression.pruned_messages_removed, None);
    assert!(!round.has_pending_pre_start_context_updates);
}

#[test]
fn start_event_preserves_current_cycle_pre_start_prune_only_state() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-start-prestart-prune-only-{}", now_epoch());

    rt.block_on(async {
        state.live_rounds.lock().await.insert(
            session_id.clone(),
            LiveRoundState {
                connection_id: 1,
                latest_compression: LiveCompressionState {
                    pruned_messages_removed: Some(3),
                    ..Default::default()
                },
                has_pending_pre_start_context_updates: true,
                ..Default::default()
            },
        );

        dispatch_live_event(
            &state,
            &session_id,
            1,
            json!({
                "type": "start",
                "round": 2,
                "phase": "analyze",
                "cycle": 5,
                "react_visible": true,
                "model": "openai/gpt-4o-reasoner",
                "think_level": "high"
            }),
        )
        .await;
    });

    let round = rt.block_on(async {
        state
            .live_rounds
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .expect("live round should exist after start")
    });

    assert_eq!(round.latest_compression.outcome, None);
    assert_eq!(round.latest_compression.pruned_messages_removed, Some(3));
    assert!(!round.has_pending_pre_start_context_updates);
}

#[test]
fn replay_live_round_replays_current_cycle_pre_start_compression_after_start() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-prestart-compression-replay-{}", now_epoch());
    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(8);

    rt.block_on(async {
        state.live_rounds.lock().await.insert(
            session_id.clone(),
            LiveRoundState {
                connection_id: 1,
                round: 2,
                react_visible: true,
                phase: Some("analyze".into()),
                cycle: Some(5),
                effective_model: Some("openai/gpt-4o-reasoner".into()),
                effective_think: Some("high".into()),
                latest_compression: LiveCompressionState {
                    outcome: Some("compressed".to_string()),
                    reason: None,
                    messages_removed: Some(4),
                    before_estimate: Some(5_000),
                    after_estimate: Some(4_000),
                    saved_tokens: Some(1_000),
                    saved_percent: Some(20),
                    pruned_messages_removed: Some(3),
                },
                assistant_text: "hello replay".to_string(),
                ..Default::default()
            },
        );
    });

    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let first = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("first event should arrive before timeout")
            .expect("first event should be queued")
    });
    let second = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("second event should arrive before timeout")
            .expect("second event should be queued")
    });
    let third = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("third event should arrive before timeout")
            .expect("third event should be queued")
    });
    let fourth = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("fourth event should arrive before timeout")
            .expect("fourth event should be queued")
    });

    let first: serde_json::Value = serde_json::from_str(&first).expect("first should be json");
    let second: serde_json::Value = serde_json::from_str(&second).expect("second should be json");
    let third: serde_json::Value = serde_json::from_str(&third).expect("third should be json");
    let fourth: serde_json::Value = serde_json::from_str(&fourth).expect("fourth should be json");

    assert_eq!(first["type"], "context_compressed");
    assert_eq!(first["messages_removed"], 4);
    assert_eq!(second["type"], "context_pruned");
    assert_eq!(second["messages_removed"], 3);
    assert_eq!(third["type"], "start");
    assert_eq!(fourth["type"], "delta");
}

#[test]
fn replay_live_round_does_not_replay_stale_compression_after_cycle_boundary() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-cycle-boundary-replay-{}", now_epoch());
    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(8);

    rt.block_on(async {
        state.live_rounds.lock().await.insert(
            session_id.clone(),
            LiveRoundState {
                connection_id: 1,
                round: 2,
                react_visible: true,
                phase: Some("analyze".into()),
                cycle: Some(5),
                effective_model: Some("openai/gpt-4o-reasoner".into()),
                effective_think: Some("high".into()),
                latest_compression: LiveCompressionState {
                    pruned_messages_removed: Some(3),
                    ..Default::default()
                },
                assistant_text: "hello replay".to_string(),
                ..Default::default()
            },
        );
    });

    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let first = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("first event should arrive before timeout")
            .expect("first event should be queued")
    });
    let second = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("second event should arrive before timeout")
            .expect("second event should be queued")
    });
    let third = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), replay_rx.recv())
            .await
            .expect("third event should arrive before timeout")
            .expect("third event should be queued")
    });

    let first: serde_json::Value = serde_json::from_str(&first).expect("first should be json");
    let second: serde_json::Value = serde_json::from_str(&second).expect("second should be json");
    let third: serde_json::Value = serde_json::from_str(&third).expect("third should be json");

    assert_eq!(first["type"], "context_pruned");
    assert_eq!(first["messages_removed"], 3);
    assert_eq!(second["type"], "start");
    assert_eq!(third["type"], "delta");
}

#[test]
fn compression_source_text_redacts_exec_tool_call_arguments() {
    let messages = vec![
        make_message("system", "system"),
        ChatMessage {
            role: "assistant".into(),
            content: Some("let me run a command".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "exec_call_1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: r#"{"command":"curl -H \"Authorization: Bearer super-secret\" --api-key \"key-123\" TOKEN=\"value\""}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let source = build_compression_source_text(&messages);

    assert!(source.contains("[REDACTED]"));
    assert!(!source.contains("super-secret"));
    assert!(!source.contains("key-123"));
    assert!(!source.contains("TOKEN=\"value\""));
}

#[test]
fn apply_live_compression_event_clears_stale_pruned_state_on_new_compression() {
    let mut round = LiveRoundState {
        latest_compression: LiveCompressionState {
            pruned_messages_removed: Some(3),
            ..Default::default()
        },
        ..Default::default()
    };

    let event = json!({
        "type": "context_compressed",
        "messages_removed": 4,
        "before_estimate": 5_000,
        "after_estimate": 4_000,
        "saved_tokens": 1_000,
        "saved_percent": 20,
    });

    apply_live_compression_event(&mut round, "context_compressed", &event);

    assert_eq!(
        round.latest_compression.outcome.as_deref(),
        Some("compressed")
    );
    assert_eq!(round.latest_compression.messages_removed, Some(4));
    assert_eq!(round.latest_compression.before_estimate, Some(5_000));
    assert_eq!(round.latest_compression.after_estimate, Some(4_000));
    assert_eq!(round.latest_compression.saved_tokens, Some(1_000));
    assert_eq!(round.latest_compression.saved_percent, Some(20));
    assert_eq!(round.latest_compression.pruned_messages_removed, None);
}

#[test]
fn apply_live_compression_event_clears_stale_pruned_state_on_skipped_compression() {
    let mut round = LiveRoundState {
        latest_compression: LiveCompressionState {
            pruned_messages_removed: Some(3),
            ..Default::default()
        },
        ..Default::default()
    };

    let event = json!({
        "type": "context_compress_skipped",
        "reason": "insufficient_savings",
    });

    apply_live_compression_event(&mut round, "context_compress_skipped", &event);

    assert_eq!(round.latest_compression.outcome.as_deref(), Some("skipped"));
    assert_eq!(
        round.latest_compression.reason.as_deref(),
        Some("insufficient_savings")
    );
    assert_eq!(round.latest_compression.messages_removed, None);
    assert_eq!(round.latest_compression.before_estimate, None);
    assert_eq!(round.latest_compression.after_estimate, None);
    assert_eq!(round.latest_compression.saved_tokens, None);
    assert_eq!(round.latest_compression.saved_percent, None);
    assert_eq!(round.latest_compression.pruned_messages_removed, None);
}

#[test]
fn apply_live_compression_event_clears_stale_pruned_state_on_failed_compression() {
    let mut round = LiveRoundState {
        latest_compression: LiveCompressionState {
            pruned_messages_removed: Some(3),
            ..Default::default()
        },
        ..Default::default()
    };

    let event = json!({
        "type": "context_compress_failed",
        "error": "network timeout",
    });

    apply_live_compression_event(&mut round, "context_compress_failed", &event);

    assert_eq!(round.latest_compression.outcome.as_deref(), Some("failed"));
    assert_eq!(
        round.latest_compression.reason.as_deref(),
        Some("network timeout")
    );
    assert_eq!(round.latest_compression.messages_removed, None);
    assert_eq!(round.latest_compression.before_estimate, None);
    assert_eq!(round.latest_compression.after_estimate, None);
    assert_eq!(round.latest_compression.saved_tokens, None);
    assert_eq!(round.latest_compression.saved_percent, None);
    assert_eq!(round.latest_compression.pruned_messages_removed, None);
}

#[test]
fn compression_replay_event_restores_compressed_state() {
    let round = LiveRoundState {
        latest_compression: LiveCompressionState {
            outcome: Some("compressed".to_string()),
            reason: None,
            messages_removed: Some(4),
            before_estimate: Some(5_000),
            after_estimate: Some(4_000),
            saved_tokens: Some(1024),
            saved_percent: Some(18),
            pruned_messages_removed: None,
        },
        ..Default::default()
    };

    let event = compression_replay_event(&round).expect("compression replay event should exist");
    assert_eq!(event["type"].as_str(), Some("context_compressed"));
    assert_eq!(event["messages_removed"].as_u64(), Some(4));
    assert_eq!(event["before_estimate"].as_u64(), Some(5_000));
    assert_eq!(event["after_estimate"].as_u64(), Some(4_000));
    assert_eq!(event["saved_tokens"].as_u64(), Some(1024));
    assert_eq!(event["saved_percent"].as_u64(), Some(18));
}

#[test]
fn compression_replay_event_restores_skipped_state() {
    let round = LiveRoundState {
        latest_compression: LiveCompressionState {
            outcome: Some("skipped".to_string()),
            reason: Some("insufficient_savings".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };

    let event = compression_replay_event(&round).expect("compression replay event should exist");
    assert_eq!(event["type"].as_str(), Some("context_compress_skipped"));
    assert_eq!(event["reason"].as_str(), Some("insufficient_savings"));
}

#[test]
fn apply_context_compressed_metrics_recomputes_saved_fields() {
    let mut event = json!({
        "type": "context_compressed",
        "before_estimate": 5_000,
        "after_estimate": 4_500,
        "saved_tokens": 500,
        "saved_percent": 10,
        "compression_ratio": 90,
    });

    crate::hooks::apply_context_compressed_metrics(&mut event, 5_000, 4_000);

    assert_eq!(event["before_estimate"].as_u64(), Some(5_000));
    assert_eq!(event["after_estimate"].as_u64(), Some(4_000));
    assert_eq!(event["saved_tokens"].as_u64(), Some(1_000));
    assert_eq!(event["saved_percent"].as_u64(), Some(20));
    assert_eq!(event["compression_ratio"].as_u64(), Some(80));
}

#[test]
fn compression_pruned_replay_event_restores_prune_state() {
    let round = LiveRoundState {
        latest_compression: LiveCompressionState {
            pruned_messages_removed: Some(3),
            ..Default::default()
        },
        ..Default::default()
    };

    let event = compression_pruned_replay_event(&round).expect("prune replay event should exist");
    assert_eq!(event["type"].as_str(), Some("context_pruned"));
    assert_eq!(event["messages_removed"].as_u64(), Some(3));
}

#[test]
fn context_compress_skipped_event_includes_reason() {
    let event = crate::hooks::build_context_compress_skipped_event("insufficient_savings");

    assert_eq!(event["type"].as_str(), Some("context_compress_skipped"));
    assert_eq!(event["reason"].as_str(), Some("insufficient_savings"));
}

#[test]
fn context_compressed_event_includes_savings_metrics() {
    let event = crate::hooks::build_context_compressed_event(4, 5_000, 4_000, 320, true);

    assert_eq!(event["type"].as_str(), Some("context_compressed"));
    assert_eq!(event["messages_removed"].as_u64(), Some(4));
    assert_eq!(event["before_estimate"].as_u64(), Some(5_000));
    assert_eq!(event["after_estimate"].as_u64(), Some(4_000));
    assert_eq!(event["saved_tokens"].as_u64(), Some(1_000));
    assert_eq!(event["saved_percent"].as_u64(), Some(20));
    assert_eq!(event["compression_ratio"].as_u64(), Some(80));
    assert_eq!(event["incremental"].as_bool(), Some(true));
}

#[test]
fn compression_saves_enough_requires_absolute_and_relative_savings() {
    assert!(crate::hooks::compression_saves_enough(5_000, 4_000));
    assert!(!crate::hooks::compression_saves_enough(5_000, 4_800));
    assert!(!crate::hooks::compression_saves_enough(2_000, 1_780));
}

#[test]
fn should_auto_compress_uses_request_budget_when_available() {
    let messages = vec![
        make_message("system", "system"),
        make_message("user", &"Q".repeat(5000)),
        make_message("assistant", &"A".repeat(5000)),
        make_message("user", &"Q2".repeat(5000)),
        make_message("assistant", &"A2".repeat(5000)),
        make_message("user", &"Q3".repeat(5000)),
        make_message("assistant", &"A3".repeat(5000)),
        make_message("user", &"Q4".repeat(5000)),
        make_message("assistant", &"A4".repeat(5000)),
        make_message("user", &"Q5".repeat(5000)),
        make_message("assistant", &"A5".repeat(5000)),
        make_message("user", &"Q6".repeat(5000)),
        make_message("assistant", &"A6".repeat(5000)),
        make_message("user", &"Q7".repeat(5000)),
        make_message("assistant", &"A7".repeat(5000)),
        make_message("user", &"Q8".repeat(5000)),
        make_message("assistant", &"A8".repeat(5000)),
        make_message("user", &"Q9".repeat(5000)),
        make_message("assistant", &"A9".repeat(5000)),
    ];
    let input = crate::hooks::HookInput {
        messages: messages.clone(),
        model: "openai/gpt-4o-mini".into(),
        provider: Provider::OpenAI,
        workspace: PathBuf::new(),
        input_budget: usize::MAX,
        request_budget: Some(crate::context::estimate_request_tokens_for_provider(
            Provider::OpenAI,
            &messages,
            &[],
        )),
        compression_extra_tools: Some(Vec::new()),
        cycle: 0,
        compression_context: None,
    };

    assert!(crate::hooks::should_auto_compress(&input, 8, 90));
}

#[test]
fn should_auto_compress_falls_back_to_message_budget_without_request_budget() {
    let messages = vec![
        make_message("system", "system"),
        make_message("user", "u1"),
        make_message("assistant", "a1"),
        make_message("user", "u2"),
        make_message("assistant", "a2"),
        make_message("user", "u3"),
        make_message("assistant", "a3"),
        make_message("user", "u4"),
        make_message("assistant", "a4"),
        make_message("user", "u5"),
        make_message("assistant", "a5"),
        make_message("user", "u6"),
        make_message("assistant", "a6"),
        make_message("user", "u7"),
        make_message("assistant", "a7"),
        make_message("user", "u8"),
        make_message("assistant", "a8"),
        make_message("user", "u9"),
        make_message("assistant", "a9"),
    ];
    let message_estimate = crate::estimate_tokens_for_provider(Provider::OpenAI, &messages);
    let input = crate::hooks::HookInput {
        messages,
        model: "openai/gpt-4o-mini".into(),
        provider: Provider::OpenAI,
        workspace: PathBuf::new(),
        input_budget: message_estimate,
        request_budget: None,
        compression_extra_tools: None,
        cycle: 0,
        compression_context: None,
    };

    assert!(crate::hooks::should_auto_compress(&input, 8, 90));
}

#[test]
fn compression_source_text_with_context_prepends_structured_sections() {
    let messages = vec![
        make_message("system", "system"),
        make_message("user", "question"),
        make_message("assistant", "answer"),
    ];
    let context = crate::hooks::CompressionContextSections {
        task_state: Some("## Task State\n- Goal: inspect runtime loop".into()),
        observation_hint: Some(
            "## Recent Observation Notes\n- read_file returned 900 lines".into(),
        ),
        task_memory: Some("## Relevant Past Experience\n- Focus: prior blockers".into()),
    };

    let source =
        crate::hooks::build_compression_source_text_with_context(&messages, Some(&context));

    assert!(source.starts_with("## Task State"));
    assert!(source.contains("## Recent Observation Notes"));
    assert!(source.contains("## Relevant Past Experience"));
    assert!(source.contains("User: question"));
    assert!(source.contains("Assistant: answer"));
}

#[test]
fn compression_call_prompt_wraps_previous_summary_for_incremental_merge() {
    let messages = vec![
        make_message("system", "system"),
        build_auto_summary_message("summary of early conversation"),
        make_message("user", "new question"),
        make_message("assistant", "new answer"),
    ];

    let prompt = crate::hooks::build_compression_call_prompt(&messages)
        .expect("prompt should be created for non-empty source");

    assert_eq!(prompt.len(), 2);
    let system = prompt[0].content.as_deref().unwrap();
    let user = prompt[1].content.as_deref().unwrap();
    assert!(system.contains("Merge them into a single updated summary"));
    assert!(user.contains("## Previous Summary"));
    assert!(user.contains("summary of early conversation"));
    assert!(user.contains("## New Conversation To Merge"));
    assert!(user.contains("new question"));
    assert!(user.contains("new answer"));
}

#[test]
fn compression_call_prompt_uses_fresh_summary_prompt_without_previous_summary() {
    let messages = vec![
        make_message("system", "system"),
        make_message("user", "question"),
        make_message("assistant", "answer"),
    ];

    let prompt = crate::hooks::build_compression_call_prompt(&messages)
        .expect("prompt should be created for non-empty source");

    assert_eq!(prompt.len(), 2);
    let system = prompt[0].content.as_deref().unwrap();
    let user = prompt[1].content.as_deref().unwrap();
    assert!(!system.contains("Merge them into a single updated summary"));
    assert!(!user.contains("## Previous Summary"));
    assert!(user.contains("question"));
    assert!(user.contains("answer"));
}

#[test]
fn compression_call_prompt_returns_none_for_empty_source() {
    let messages = vec![make_message("system", "system")];

    assert!(crate::hooks::build_compression_call_prompt(&messages).is_none());
}

#[test]
fn resolve_model_uses_config_for_plain_model_id() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
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
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let resolved = config.resolve_model("gpt-4o-mini");

    assert_eq!(resolved.model_id, "gpt-4o-mini");
    assert_eq!(resolved.api_base, "https://api.openai.com/v1");
    assert_eq!(resolved.api_key, "test-key");
    assert_eq!(resolved.max_tokens, Some(16384));
    assert_eq!(resolved.context_window, 128000);
}

#[test]
fn legacy_settings_provider_fields_deserialize() {
    let cfg: JsonConfig = serde_json::from_str(
        r#"{
            "settings": {
                "port": 3001,
                "provider": "anthropic",
                "apiKey": "legacy-key",
                "apiBase": "https://legacy.example",
                "execTimeout": 12,
                "maxContextTokens": 64000,
                "maxOutputBytes": 1024,
                "maxFileBytes": 2048
            }
        }"#,
    )
    .expect("legacy settings fields should deserialize for backward compatibility");

    let settings = cfg.settings.expect("settings should deserialize");
    assert_eq!(settings.port, Some(3001));
    assert_eq!(settings.provider.as_deref(), Some("anthropic"));
    assert_eq!(settings.api_key.as_deref(), Some("legacy-key"));
    assert_eq!(settings.api_base.as_deref(), Some("https://legacy.example"));
    assert_eq!(settings.exec_timeout, Some(12));
    assert_eq!(settings.max_context_tokens, Some(64000));
    assert_eq!(settings.max_output_bytes, Some(1024));
    assert_eq!(settings.max_file_bytes, Some(2048));
}

#[test]
fn settings_openai_stream_include_usage_deserializes() {
    let cfg: JsonConfig = serde_json::from_str(
        r#"{
            "settings": {
                "openaiStreamIncludeUsage": true
            }
        }"#,
    )
    .expect("openaiStreamIncludeUsage should deserialize");

    let settings = cfg.settings.expect("settings should deserialize");
    assert_eq!(settings.openai_stream_include_usage, Some(true));
}

#[test]
fn settings_enable_s3_deserializes() {
    let cfg: JsonConfig = serde_json::from_str(
        r#"{
            "settings": {
                "enableS3": true
            }
        }"#,
    )
    .expect("enableS3 should deserialize");

    let settings = cfg.settings.expect("settings should deserialize");
    assert_eq!(settings.enable_s3, Some(true));
}

#[test]
fn effective_enable_s3_prefers_env_override() {
    assert_eq!(
        crate::config::effective_enable_s3(Some(false), Some(true)),
        Some(true)
    );
    assert_eq!(
        crate::config::effective_enable_s3(Some(true), Some(false)),
        Some(false)
    );
    assert_eq!(
        crate::config::effective_enable_s3(Some(true), None),
        Some(true)
    );
    assert_eq!(
        crate::config::effective_enable_s3(None, Some(false)),
        Some(false)
    );
}

#[test]
fn settings_tool_timeout_deserializes() {
    let cfg: JsonConfig = serde_json::from_str(
        r#"{
            "settings": {
                "toolTimeout": 45
            }
        }"#,
    )
    .expect("toolTimeout should deserialize");

    let settings = cfg.settings.expect("settings should deserialize");
    assert_eq!(settings.tool_timeout, Some(45));
}

#[test]
fn settings_daily_reflection_deserializes() {
    let cfg: JsonConfig = serde_json::from_str(
        r#"{
            "settings": {
                "dailyReflection": true
            }
        }"#,
    )
    .expect("dailyReflection should deserialize");

    let settings = cfg.settings.expect("settings should deserialize");
    assert_eq!(settings.daily_reflection, Some(true));
}

#[test]
fn reflection_model_config_deserializes() {
    let cfg: JsonConfig = serde_json::from_str(
        r#"{
            "agents": {
                "defaults": {
                    "model": {
                        "primary": "gpt-4o",
                        "reflection": "gpt-4o-mini"
                    }
                }
            }
        }"#,
    )
    .expect("reflection model should deserialize");

    let model = cfg.agents.unwrap().defaults.unwrap().model.unwrap();
    assert_eq!(model.reflection.as_deref(), Some("gpt-4o-mini"));
}

#[test]
fn settings_sub_agent_timeout_deserializes() {
    let cfg: JsonConfig = serde_json::from_str(
        r#"{
            "settings": {
                "subAgentTimeout": 600
            }
        }"#,
    )
    .expect("subAgentTimeout should deserialize");

    let settings = cfg.settings.expect("settings should deserialize");
    assert_eq!(settings.sub_agent_timeout, Some(600));
}

#[test]
fn settings_sub_agent_timeout_zero_means_unlimited() {
    let cfg: JsonConfig = serde_json::from_str(
        r#"{
            "settings": {
                "subAgentTimeout": 0
            }
        }"#,
    )
    .expect("subAgentTimeout=0 should deserialize");

    let settings = cfg.settings.expect("settings should deserialize");
    assert_eq!(settings.sub_agent_timeout, Some(0));
}

#[test]
fn format_sub_agent_timeout_renders_unlimited_for_zero() {
    assert_eq!(
        crate::config::format_sub_agent_timeout(Duration::ZERO),
        "unlimited"
    );
}

#[test]
fn format_sub_agent_timeout_renders_seconds_when_nonzero() {
    assert_eq!(
        crate::config::format_sub_agent_timeout(Duration::from_secs(7)),
        "7s"
    );
}

#[test]
fn settings_max_llm_retries_deserializes() {
    let cfg: JsonConfig = serde_json::from_str(
        r#"{
            "settings": {
                "maxLlmRetries": 5
            }
        }"#,
    )
    .expect("maxLlmRetries should deserialize");

    let settings = cfg.settings.expect("settings should deserialize");
    assert_eq!(settings.max_llm_retries, Some(5));
}

#[test]
fn settings_max_llm_retries_defaults_to_none() {
    let cfg: JsonConfig =
        serde_json::from_str(r#"{"settings": {}}"#).expect("empty settings should deserialize");

    let settings = cfg.settings.expect("settings should deserialize");
    assert_eq!(settings.max_llm_retries, None);
}

#[test]
fn settings_anthropic_prompt_caching_deserializes() {
    let cfg: JsonConfig = serde_json::from_str(
        r#"{
            "settings": {
                "anthropicPromptCaching": true
            }
        }"#,
    )
    .expect("anthropicPromptCaching should deserialize");

    let settings = cfg.settings.expect("settings should deserialize");
    assert_eq!(settings.anthropic_prompt_caching, Some(true));
}

#[test]
fn build_history_payload_preserves_raw_tool_result_content() {
    let long_raw_result = format!("{{\"ok\":true,\"payload\":\"{}\"}}", "x".repeat(5000));
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![
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
                role: "tool".into(),
                content: Some(long_raw_result.clone()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some("call_1".into()),
                timestamp: Some(123),
            },
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: 0,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let messages = payload["messages"]
        .as_array()
        .expect("history payload should contain a messages array");
    let tool_result = messages
        .iter()
        .find(|message| message["role"] == "tool_result")
        .expect("history payload should contain a tool_result entry");

    assert_eq!(
        tool_result["result"].as_str(),
        Some(long_raw_result.as_str())
    );
    assert_eq!(tool_result["is_error"].as_bool(), Some(false));
}

#[test]
fn build_history_payload_marks_failed_tool_result_with_is_error() {
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![ChatMessage {
            role: "tool".into(),
            content: Some("Sub-agent 'coder' timed out after 30s".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("task_1".into()),
            timestamp: Some(123),
        }],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: HashSet::from(["task_1".to_string()]),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let messages = payload["messages"]
        .as_array()
        .expect("history payload should contain a messages array");
    let tool_result = messages
        .iter()
        .find(|message| message["role"] == "tool_result")
        .expect("history payload should contain a tool_result entry");

    assert_eq!(tool_result["id"].as_str(), Some("task_1"));
    assert_eq!(tool_result["is_error"].as_bool(), Some(true));
}

#[test]
fn build_history_payload_hides_internal_image_cache_metadata() {
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some("look".into()),
            images: Some(vec![ImageAttachment {
                url: "https://example.com/photo.png".into(),
                s3_object_key: None,
                cache_path: Some("C:/internal/cache/file.b64".into()),
                data: Some("aW1hZ2U=".into()),
            }]),
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(123),
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let images = payload["messages"][0]["images"]
        .as_array()
        .expect("images should be present");
    assert_eq!(images.len(), 1);
    assert_eq!(images[0]["url"], "https://example.com/photo.png");
    assert!(images[0].get("cache_path").is_none());
    assert!(images[0].get("data").is_none());
}

#[test]
fn build_history_payload_with_s3_refreshes_uploaded_image_urls() {
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![ChatMessage {
            role: "user".into(),
            content: Some("look".into()),
            images: Some(vec![ImageAttachment {
                url: "https://expired.example.test/photo.png".into(),
                s3_object_key: Some("lingclaw/images/2026/demo.png".into()),
                cache_path: None,
                data: None,
            }]),
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(123),
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };
    let s3_cfg = crate::config::S3Config {
        endpoint: "https://minio.example.test/storage".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "lingclaw/images/".into(),
        url_expiry_secs: 3600,
        lifecycle_days: 14,
    };

    let payload = crate::session_store::build_history_payload_with_s3(&session, Some(&s3_cfg));
    let url = payload["messages"][0]["images"][0]["url"]
        .as_str()
        .expect("history image url should exist");

    assert!(
        url.starts_with("https://minio.example.test/storage/bucket/lingclaw/images/2026/demo.png?")
    );
    assert!(url.contains("X-Amz-Signature="));
}

#[test]
fn build_history_payload_includes_thinking_only_assistant_messages() {
    // An assistant message that has thinking but no content (e.g., think -> tool_call
    // cycle with no text response) must appear in the history payload so that the
    // reasoning card is replayed after a page refresh.
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![
            ChatMessage {
                role: "assistant".into(),
                content: None, // no text - only thinking + tool_calls
                images: None,
                thinking: Some("step by step reasoning".into()),
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![crate::ToolCall {
                    id: "call_abc".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "exec".into(),
                        arguments: "{}".into(),
                    },
                }]),
                tool_call_id: None,
                timestamp: Some(1000),
            },
            ChatMessage {
                role: "assistant".into(),
                content: Some("done".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(2000),
            },
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let msgs = payload["messages"].as_array().unwrap();

    // The thinking-only assistant entry must appear.
    let thinking_entry = msgs
        .iter()
        .find(|m| m["role"] == "assistant" && m.get("thinking").is_some())
        .expect("history should contain the thinking-only assistant entry");
    assert_eq!(
        thinking_entry["thinking"].as_str(),
        Some("step by step reasoning")
    );
    // Content should be present as an empty string (not omitted).
    assert_eq!(thinking_entry["content"].as_str(), Some(""));

    // The second assistant entry (with actual content) should also be present.
    let content_entry = msgs
        .iter()
        .find(|m| m["role"] == "assistant" && m["content"] == "done")
        .expect("history should contain the content assistant entry");
    assert!(content_entry.get("thinking").is_none());
}

#[test]
fn build_history_payload_redacts_exec_tool_call_arguments() {
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![crate::ToolCall {
                id: "exec_call_1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: r#"{"command":"curl -H \"Authorization: Bearer super-secret\" --api-key \"key-123\" TOKEN=\"value\"","working_dir":"src"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: Some(1000),
        }],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let tool_call = payload["messages"]
        .as_array()
        .expect("history messages should be an array")
        .iter()
        .find(|message| message["role"] == "tool_call" && message["id"] == "exec_call_1")
        .expect("exec tool_call should be present");
    let arguments = tool_call["arguments"]
        .as_str()
        .expect("tool_call arguments should be a string");

    assert!(arguments.contains("[REDACTED]"));
    assert!(!arguments.contains("super-secret"));
    assert!(!arguments.contains("key-123"));
    assert!(!arguments.contains("TOKEN=\"value\""));
}

#[test]
fn build_history_payload_includes_subagent_snapshot_on_task_results() {
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![
            ChatMessage {
                role: "assistant".into(),
                content: None,
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![crate::ToolCall {
                    id: "task_call_1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "task".into(),
                        arguments: r#"{"agent":"reviewer","prompt":"Inspect logs"}"#.into(),
                    },
                }]),
                tool_call_id: None,
                timestamp: Some(1000),
            },
            ChatMessage {
                role: "tool".into(),
                content: Some("Found the issue in the logs.".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some("task_call_1".into()),
                timestamp: Some(1001),
            },
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::from([(
            subagent_snapshot_storage_key("task_call_1", 1),
            SubagentHistorySnapshot {
                reasoning: Some("[Cycle 1]\nInspect logs".into()),
                tools: vec![SubagentToolHistorySnapshot {
                    id: "tool-1".into(),
                    name: "read_file".into(),
                    arguments: Some(r#"{"path":"logs/app.log"}"#.into()),
                    result: Some("panic: startup config missing".into()),
                    is_error: false,
                    duration_ms: 12,
                }],
                cycles: 1,
                tool_calls: 1,
                duration_ms: 120,
                input_tokens: 55,
                output_tokens: 21,
                success: true,
                result_excerpt: Some("Found the issue in the logs.".into()),
                error: None,
            },
        )]),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let msgs = payload["messages"]
        .as_array()
        .expect("history messages should be an array");
    let tool_result = msgs
        .iter()
        .find(|message| message["role"] == "tool_result" && message["id"] == "task_call_1")
        .expect("task tool_result should be present");

    assert_eq!(
        tool_result["subagent_snapshot"]["reasoning"].as_str(),
        Some("[Cycle 1]\nInspect logs")
    );
    assert_eq!(
        tool_result["subagent_snapshot"]["tools"][0]["name"].as_str(),
        Some("read_file")
    );
    assert_eq!(
        tool_result["subagent_snapshot"]["result_excerpt"].as_str(),
        Some("Found the issue in the logs.")
    );
}

#[test]
fn build_history_payload_redacts_exec_args_in_subagent_snapshot() {
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![
            ChatMessage {
                role: "assistant".into(),
                content: None,
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![crate::ToolCall {
                    id: "task_call_1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "task".into(),
                        arguments: r#"{"agent":"reviewer","prompt":"Inspect logs"}"#.into(),
                    },
                }]),
                tool_call_id: None,
                timestamp: Some(1000),
            },
            ChatMessage {
                role: "tool".into(),
                content: Some("Found the issue in the logs.".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some("task_call_1".into()),
                timestamp: Some(1001),
            },
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::from([(
            subagent_snapshot_storage_key("task_call_1", 1),
            SubagentHistorySnapshot {
                reasoning: Some("[Cycle 1]\nInspect logs".into()),
                tools: vec![SubagentToolHistorySnapshot {
                    id: "tool-1".into(),
                    name: "exec".into(),
                    arguments: Some(
                        r#"{"command":"curl -H \"Authorization: Bearer super-secret\" --api-key \"key-123\" TOKEN=\"value\""}"#
                            .into(),
                    ),
                    result: Some("ok".into()),
                    is_error: false,
                    duration_ms: 12,
                }],
                cycles: 1,
                tool_calls: 1,
                duration_ms: 120,
                input_tokens: 55,
                output_tokens: 21,
                success: true,
                result_excerpt: Some("Found the issue in the logs.".into()),
                error: None,
            },
        )]),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let tool_result = payload["messages"]
        .as_array()
        .expect("history messages should be an array")
        .iter()
        .find(|message| message["role"] == "tool_result" && message["id"] == "task_call_1")
        .expect("task tool_result should be present");
    let arguments = tool_result["subagent_snapshot"]["tools"][0]["arguments"]
        .as_str()
        .expect("snapshot tool arguments should be a string");

    assert!(arguments.contains("[REDACTED]"));
    assert!(!arguments.contains("super-secret"));
    assert!(!arguments.contains("key-123"));
    assert!(!arguments.contains("TOKEN=\"value\""));
}

#[test]
fn build_history_payload_normalizes_legacy_subagent_snapshot_keys() {
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![
            ChatMessage {
                role: "assistant".into(),
                content: None,
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![crate::ToolCall {
                    id: "task_call_1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "task".into(),
                        arguments: r#"{"agent":"reviewer","prompt":"Inspect logs"}"#.into(),
                    },
                }]),
                tool_call_id: None,
                timestamp: Some(1000),
            },
            ChatMessage {
                role: "tool".into(),
                content: Some("Found the issue in the logs.".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some("task_call_1".into()),
                timestamp: Some(1001),
            },
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::from([(
            "task_call_1".to_string(),
            SubagentHistorySnapshot {
                result_excerpt: Some("Found the issue in the logs.".into()),
                success: true,
                ..Default::default()
            },
        )]),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let msgs = payload["messages"]
        .as_array()
        .expect("history messages should be an array");
    let tool_result = msgs
        .iter()
        .find(|message| message["role"] == "tool_result" && message["id"] == "task_call_1")
        .expect("task tool_result should be present");

    assert_eq!(
        tool_result["subagent_snapshot"]["result_excerpt"].as_str(),
        Some("Found the issue in the logs.")
    );
}

#[test]
fn build_history_payload_distinguishes_repeated_task_tool_call_ids() {
    let first_snapshot = SubagentHistorySnapshot {
        result_excerpt: Some("First delegated result".into()),
        success: true,
        ..Default::default()
    };
    let second_snapshot = SubagentHistorySnapshot {
        result_excerpt: Some("Second delegated result".into()),
        success: true,
        ..Default::default()
    };
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![
            ChatMessage {
                role: "assistant".into(),
                content: None,
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![crate::ToolCall {
                    id: "call_1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "task".into(),
                        arguments: r#"{"agent":"reviewer","prompt":"Inspect logs"}"#.into(),
                    },
                }]),
                tool_call_id: None,
                timestamp: Some(1000),
            },
            ChatMessage {
                role: "tool".into(),
                content: Some("First delegated result".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some("call_1".into()),
                timestamp: Some(1001),
            },
            ChatMessage {
                role: "assistant".into(),
                content: None,
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![crate::ToolCall {
                    id: "call_1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "task".into(),
                        arguments: r#"{"agent":"reviewer","prompt":"Inspect newer logs"}"#.into(),
                    },
                }]),
                tool_call_id: None,
                timestamp: Some(1002),
            },
            ChatMessage {
                role: "tool".into(),
                content: Some("Second delegated result".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some("call_1".into()),
                timestamp: Some(1003),
            },
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 2,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::from([
            (
                subagent_snapshot_storage_key("call_1", 1),
                first_snapshot.clone(),
            ),
            (
                subagent_snapshot_storage_key("call_1", 2),
                second_snapshot.clone(),
            ),
        ]),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let results: Vec<_> = payload["messages"]
        .as_array()
        .expect("history messages should be an array")
        .iter()
        .filter(|message| message["role"] == "tool_result" && message["id"] == "call_1")
        .collect();

    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0]["subagent_snapshot"]["result_excerpt"].as_str(),
        Some("First delegated result")
    );
    assert_eq!(
        results[1]["subagent_snapshot"]["result_excerpt"].as_str(),
        Some("Second delegated result")
    );
}

#[test]
fn replace_session_messages_rekeys_subagent_snapshots_for_remaining_history() {
    let assistant_first = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![crate::ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "task".into(),
                arguments: r#"{"agent":"reviewer","prompt":"Inspect logs"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: Some(1000),
    };
    let tool_first = ChatMessage {
        role: "tool".into(),
        content: Some("First delegated result".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: Some("call_1".into()),
        timestamp: Some(1001),
    };
    let assistant_second = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![crate::ToolCall {
            id: "call_1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "task".into(),
                arguments: r#"{"agent":"reviewer","prompt":"Inspect newer logs"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: Some(1002),
    };
    let tool_second = ChatMessage {
        role: "tool".into(),
        content: Some("Second delegated result".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: Some("call_1".into()),
        timestamp: Some(1003),
    };
    let mut session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: Some("sys".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(999),
            },
            assistant_first.clone(),
            tool_first,
            assistant_second.clone(),
            tool_second,
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 2,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::from([
            (
                subagent_snapshot_storage_key("call_1", 1),
                SubagentHistorySnapshot {
                    result_excerpt: Some("First delegated result".into()),
                    success: true,
                    ..Default::default()
                },
            ),
            (
                subagent_snapshot_storage_key("call_1", 2),
                SubagentHistorySnapshot {
                    result_excerpt: Some("Second delegated result".into()),
                    success: true,
                    ..Default::default()
                },
            ),
        ]),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };

    let kept_system = session.messages[0].clone();
    let kept_tool = session.messages[4].clone();
    replace_session_messages(
        &mut session,
        vec![
            kept_system,
            build_auto_summary_message("compressed summary"),
            assistant_second,
            kept_tool,
        ],
    );

    assert_eq!(session.subagent_snapshots.len(), 1);
    assert!(
        session
            .subagent_snapshots
            .contains_key(&subagent_snapshot_storage_key("call_1", 1))
    );
    let payload = build_history_payload(&session);
    let results: Vec<_> = payload["messages"]
        .as_array()
        .expect("history messages should be an array")
        .iter()
        .filter(|message| message["role"] == "tool_result")
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["subagent_snapshot"]["result_excerpt"].as_str(),
        Some("Second delegated result")
    );
}

#[tokio::test]
async fn replace_session_todos_rejects_stale_revision_without_overwriting_snapshot() {
    let state = test_app_state();
    let session_id = format!("todos-conflict-{}", now_epoch());
    let mut session = test_session(&session_id, "Todos Conflict", None);
    session.version = SESSION_VERSION;
    session.todos = crate::todos::TodoSnapshot {
        revision: 2,
        items: vec![crate::todos::TodoItem {
            id: "todo-1".into(),
            content: "keep current".into(),
            status: crate::todos::TodoStatus::InProgress,
        }],
        last_updated_by: crate::todos::TodoUpdatedBy::User,
        updated_at: 123,
    };

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session);
    }

    let response = crate::todos::replace_session_todos(
        &state,
        &session_id,
        crate::todos::TodoReplaceRequest {
            base_revision: 1,
            items: vec![crate::todos::TodoItem {
                id: "todo-2".into(),
                content: "stale overwrite".into(),
                status: crate::todos::TodoStatus::Pending,
            }],
        },
        crate::todos::TodoUpdateOrigin::Assistant,
    )
    .await
    .expect("stale revision should return a conflict snapshot");

    assert!(!response.ok);
    assert!(response.conflict);
    assert_eq!(response.revision, 2);
    assert_eq!(response.items.len(), 1);
    assert_eq!(response.items[0].content, "keep current");

    let stored = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(&session_id)
            .expect("session should still exist")
            .todos
            .clone()
    };
    assert_eq!(stored.revision, 2);
    assert_eq!(stored.items[0].content, "keep current");

    let _ = std::fs::remove_file(sessions_dir().join(format!("{session_id}.json")));
}

#[test]
fn build_history_payload_omits_todos_tool_messages() {
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![
            make_message("system", "system"),
            ChatMessage {
                role: "assistant".into(),
                content: None,
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![ToolCall {
                    id: "todo_call_1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: crate::tools::TOOL_NAME_TODOS.into(),
                        arguments: r#"{"base_revision":0,"items":[]}"#.into(),
                    },
                }]),
                tool_call_id: None,
                timestamp: Some(1000),
            },
            ChatMessage {
                role: "tool".into(),
                content: Some(r#"{"ok":true,"conflict":false,"revision":1,"items":[]}"#.into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some("todo_call_1".into()),
                timestamp: Some(1001),
            },
            ChatMessage {
                role: "assistant".into(),
                content: None,
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![ToolCall {
                    id: "exec_call_1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "exec".into(),
                        arguments: r#"{"command":"echo ok"}"#.into(),
                    },
                }]),
                tool_call_id: None,
                timestamp: Some(1002),
            },
            ChatMessage {
                role: "tool".into(),
                content: Some("ok".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some("exec_call_1".into()),
                timestamp: Some(1003),
            },
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 2,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let messages = payload["messages"]
        .as_array()
        .expect("history messages should be an array");

    assert!(
        messages
            .iter()
            .all(|message| message["id"] != "todo_call_1")
    );
    assert!(messages.iter().any(|message| {
        message["role"] == "tool_call"
            && message["id"] == "exec_call_1"
            && message["name"] == "exec"
    }));
    assert!(
        messages
            .iter()
            .any(|message| { message["role"] == "tool_result" && message["id"] == "exec_call_1" })
    );
}

#[test]
fn provider_detect_accepts_provider_prefixed_model_refs() {
    assert_eq!(
        Provider::detect(
            "anthropic/claude-opus-4-7",
            "https://api.openai.com/v1",
            None,
        ),
        Provider::Anthropic
    );
    assert_eq!(
        Provider::detect("openai/gpt-4o-mini", "https://api.anthropic.com", None),
        Provider::OpenAI
    );
    assert_eq!(
        Provider::detect("ollama/llama3.2", "https://api.openai.com/v1", None),
        Provider::Ollama
    );
    assert_eq!(
        Provider::detect("llama3.2", "http://127.0.0.1:11434", None),
        Provider::Ollama
    );
}

#[test]
fn resolve_model_uses_ollama_provider_config_for_plain_model_id() {
    let mut providers = HashMap::new();
    providers.insert(
        "ollama".to_string(),
        JsonProviderConfig {
            base_url: "http://127.0.0.1:11434".to_string(),
            api_key: String::new(),
            api: "ollama".to_string(),
            models: vec![JsonModelEntry {
                id: "llama3.2".to_string(),
                name: None,
                reasoning: Some(true),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(8192),
                compat: Some(json!({"thinkingFormat": "ollama"})),
            }],
        },
    );

    let config = Config {
        api_key: String::new(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "llama3.2".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::Ollama,
        anthropic_prompt_caching: false,
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let resolved = config.resolve_model("llama3.2");

    assert_eq!(resolved.provider, Provider::Ollama);
    assert_eq!(resolved.api_base, "http://127.0.0.1:11434");
    assert_eq!(resolved.model_id, "llama3.2");
    assert_eq!(resolved.max_tokens, Some(8192));
    assert_eq!(resolved.context_window, 128000);
    assert_eq!(resolved.thinking_format.as_deref(), Some("ollama"));
}

#[test]
fn cli_default_model_marker_uses_canonical_model_ref() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai-a".to_string(),
        JsonProviderConfig {
            base_url: "https://api-a.example/v1".to_string(),
            api_key: "key-a".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(4096),
                compat: None,
            }],
        },
    );
    providers.insert(
        "openai-b".to_string(),
        JsonProviderConfig {
            base_url: "https://api-b.example/v1".to_string(),
            api_key: "key-b".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(8192),
                compat: None,
            }],
        },
    );

    let config = Config {
        api_key: "key-a".to_string(),
        api_base: "https://api-a.example/v1".to_string(),
        model: "shared-model".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::OpenAI,
        anthropic_prompt_caching: false,
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    assert!(crate::cli::is_default_model_row(
        &config,
        "openai-a",
        "shared-model"
    ));
    assert_eq!(
        config.resolved_model_ref("shared-model"),
        "openai-a/shared-model"
    );
    assert!(!crate::cli::is_default_model_row(
        &config,
        "openai-b",
        "shared-model"
    ));
}

#[test]
fn resolve_model_prefers_current_provider_for_duplicate_plain_ids() {
    let mut providers = HashMap::new();
    providers.insert(
        "anthropic".to_string(),
        JsonProviderConfig {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: "anthropic-key".to_string(),
            api: "anthropic".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(1_000_000),
                max_tokens: Some(64_000),
                compat: None,
            }],
        },
    );
    providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "openai-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
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
        api_key: "env-key".to_string(),
        api_base: "https://fallback.example/v1".to_string(),
        model: "shared-model".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::OpenAI,
        anthropic_prompt_caching: false,
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let resolved = config.resolve_model("shared-model");

    assert_eq!(resolved.provider, Provider::OpenAI);
    assert_eq!(resolved.api_base, "https://api.openai.com/v1");
    assert_eq!(resolved.api_key, "openai-key");
    assert_eq!(resolved.max_tokens, Some(16384));
}

#[test]
fn resolve_model_prefers_exact_runtime_match_for_same_provider_type() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai-a".to_string(),
        JsonProviderConfig {
            base_url: "https://api-a.example/v1".to_string(),
            api_key: "key-a".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(4096),
                compat: None,
            }],
        },
    );
    providers.insert(
        "openai-b".to_string(),
        JsonProviderConfig {
            base_url: "https://api-b.example/v1".to_string(),
            api_key: "key-b".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(8192),
                compat: None,
            }],
        },
    );

    let config = Config {
        api_key: "key-b".to_string(),
        api_base: "https://api-b.example/v1".to_string(),
        model: "shared-model".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::OpenAI,
        anthropic_prompt_caching: false,
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let resolved = config.resolve_model("shared-model");

    assert_eq!(resolved.provider, Provider::OpenAI);
    assert_eq!(resolved.api_base, "https://api-b.example/v1");
    assert_eq!(resolved.api_key, "key-b");
    assert_eq!(resolved.max_tokens, Some(8192));
}

#[test]
fn resolve_model_prefers_exact_runtime_match_for_same_anthropic_provider_type() {
    let mut providers = HashMap::new();
    providers.insert(
        "anthropic-a".to_string(),
        JsonProviderConfig {
            base_url: "https://anthropic-a.example".to_string(),
            api_key: "ant-key-a".to_string(),
            api: "anthropic".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(1_000_000),
                max_tokens: Some(64_000),
                compat: None,
            }],
        },
    );
    providers.insert(
        "anthropic-b".to_string(),
        JsonProviderConfig {
            base_url: "https://anthropic-b.example".to_string(),
            api_key: "ant-key-b".to_string(),
            api: "anthropic".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(200000),
                max_tokens: Some(12288),
                compat: None,
            }],
        },
    );

    let config = Config {
        api_key: "ant-key-b".to_string(),
        api_base: "https://anthropic-b.example".to_string(),
        model: "shared-model".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::Anthropic,
        anthropic_prompt_caching: false,
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let resolved = config.resolve_model("shared-model");

    assert_eq!(resolved.provider, Provider::Anthropic);
    assert_eq!(resolved.api_base, "https://anthropic-b.example");
    assert_eq!(resolved.api_key, "ant-key-b");
    assert_eq!(resolved.max_tokens, Some(12288));
}

#[test]
fn resolve_model_prefers_exact_runtime_match_for_same_ollama_provider_type() {
    let mut providers = HashMap::new();
    providers.insert(
        "ollama-a".to_string(),
        JsonProviderConfig {
            base_url: "http://127.0.0.1:11434".to_string(),
            api_key: "ollama-key-a".to_string(),
            api: "ollama".to_string(),
            models: vec![JsonModelEntry {
                id: "qwen3".to_string(),
                name: None,
                reasoning: Some(true),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(8192),
                compat: Some(json!({"thinkingFormat": "qwen"})),
            }],
        },
    );
    providers.insert(
        "ollama-b".to_string(),
        JsonProviderConfig {
            base_url: "http://127.0.0.1:11435".to_string(),
            api_key: "ollama-key-b".to_string(),
            api: "ollama".to_string(),
            models: vec![JsonModelEntry {
                id: "qwen3".to_string(),
                name: None,
                reasoning: Some(true),
                input: None,
                cost: None,
                context_window: Some(256000),
                max_tokens: Some(16384),
                compat: Some(json!({"thinkingFormat": "ollama"})),
            }],
        },
    );

    let config = Config {
        api_key: "ollama-key-b".to_string(),
        api_base: "http://127.0.0.1:11435".to_string(),
        model: "qwen3".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::Ollama,
        anthropic_prompt_caching: false,
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let resolved = config.resolve_model("qwen3");

    assert_eq!(resolved.provider, Provider::Ollama);
    assert_eq!(resolved.api_base, "http://127.0.0.1:11435");
    assert_eq!(resolved.api_key, "ollama-key-b");
    assert_eq!(resolved.max_tokens, Some(16384));
    assert_eq!(resolved.context_window, 256000);
    assert_eq!(resolved.thinking_format.as_deref(), Some("ollama"));
}

#[test]
fn canonical_model_ref_expands_unique_plain_id() {
    let mut providers = HashMap::new();
    providers.insert(
        "anthropic".to_string(),
        JsonProviderConfig {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: "anthropic-key".to_string(),
            api: "anthropic".to_string(),
            models: vec![JsonModelEntry {
                id: "claude-opus-4-7".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(1_000_000),
                max_tokens: Some(64_000),
                compat: None,
            }],
        },
    );

    let config = Config {
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
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let canonical = config
        .canonical_model_ref("claude-opus-4-7")
        .expect("unique model id should expand to provider/model");

    assert_eq!(canonical, "anthropic/claude-opus-4-7");
}

#[test]
fn canonical_model_ref_rejects_ambiguous_plain_id() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai-a".to_string(),
        JsonProviderConfig {
            base_url: "https://api-a.example/v1".to_string(),
            api_key: "key-a".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(4096),
                compat: None,
            }],
        },
    );
    providers.insert(
        "openai-b".to_string(),
        JsonProviderConfig {
            base_url: "https://api-b.example/v1".to_string(),
            api_key: "key-b".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(8192),
                compat: None,
            }],
        },
    );

    let config = Config {
        api_key: "key-a".to_string(),
        api_base: "https://api-a.example/v1".to_string(),
        model: "shared-model".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::OpenAI,
        anthropic_prompt_caching: false,
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let err = config
        .canonical_model_ref("shared-model")
        .expect_err("ambiguous plain model id should be rejected");

    assert!(err.contains("ambiguous"));
    assert!(err.contains("openai-a/shared-model"));
    assert!(err.contains("openai-b/shared-model"));
}

#[test]
fn available_models_omits_ambiguous_plain_default_alias() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai-a".to_string(),
        JsonProviderConfig {
            base_url: "https://api-a.example/v1".to_string(),
            api_key: "key-a".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(4096),
                compat: None,
            }],
        },
    );
    providers.insert(
        "openai-b".to_string(),
        JsonProviderConfig {
            base_url: "https://api-b.example/v1".to_string(),
            api_key: "key-b".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "shared-model".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(8192),
                compat: None,
            }],
        },
    );

    let config = Config {
        api_key: "key-a".to_string(),
        api_base: "https://api-a.example/v1".to_string(),
        model: "shared-model".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::OpenAI,
        anthropic_prompt_caching: false,
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let available = config.available_models();

    assert!(available.contains(&"openai-a/shared-model".to_string()));
    assert!(available.contains(&"openai-b/shared-model".to_string()));
    assert!(!available.contains(&"shared-model".to_string()));
}

#[test]
fn canonical_model_ref_rejects_unknown_plain_id_when_providers_exist() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "gpt-4o".to_string(),
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
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let err = config
        .canonical_model_ref("does-not-exist")
        .expect_err("unknown plain model id should be rejected");

    assert!(err.contains("Unknown model 'does-not-exist'"));
}

#[test]
fn canonical_model_ref_preserves_explicit_provider_model() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "gpt-4o".to_string(),
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
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let canonical = config
        .canonical_model_ref("openai/gpt-4o")
        .expect("configured provider/model should be preserved");

    assert_eq!(canonical, "openai/gpt-4o");
}

#[test]
fn canonical_model_ref_allows_explicit_provider_without_provider_config() {
    let config = Config {
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
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let canonical = config
        .canonical_model_ref("openai/gpt-4o-mini")
        .expect("env-only mode should allow explicit provider/model refs");

    assert_eq!(canonical, "openai/gpt-4o-mini");
}

#[test]
fn resolve_model_strips_provider_prefix_without_provider_config() {
    let config = Config {
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
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let resolved = config.resolve_model("anthropic/claude-opus-4-7");

    assert_eq!(resolved.provider, Provider::Anthropic);
    assert_eq!(resolved.api_base, "https://api.anthropic.com");
    assert_eq!(resolved.model_id, "claude-opus-4-7");
}

#[test]
fn resolve_model_accepts_ollama_prefix_without_provider_config() {
    let config = Config {
        api_key: String::new(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "llama3.2".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::Ollama,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let resolved = config.resolve_model("ollama/llama3.2");

    assert_eq!(resolved.provider, Provider::Ollama);
    assert_eq!(resolved.api_base, "http://127.0.0.1:11434");
    assert_eq!(resolved.model_id, "llama3.2");
}

#[test]
fn build_session_status_reports_resolved_target() {
    let mut providers = HashMap::new();
    providers.insert(
        "anthropic".to_string(),
        JsonProviderConfig {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: "anthropic-key".to_string(),
            api: "anthropic".to_string(),
            models: vec![JsonModelEntry {
                id: "claude-opus-4-7".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(1_000_000),
                max_tokens: Some(64_000),
                compat: None,
            }],
        },
    );

    let config = Config {
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
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };
    let mut session = test_session("abc", "Test", Some("anthropic/claude-opus-4-7"));
    session.think_level = "medium".to_string();

    let status = build_session_status(&session, &config);

    assert!(status.contains("model: anthropic/claude-opus-4-7"));
    assert!(status.contains("resolved_provider: anthropic"));
    assert!(status.contains("resolved_api_base: https://api.anthropic.com"));
    assert!(status.contains("resolved_model_id: claude-opus-4-7"));
    assert!(status.contains("max_tokens: 64K"));
    assert!(status.contains("context_est: 4/900K (limit 1M)"));
    assert!(status.contains("token_usage_source: input=estimated output=estimated"));
    assert!(status.contains("think: medium"));
}

#[test]
fn build_system_prompt_uses_cached_static_prefix_on_repeat_query() {
    let workspace = std::env::temp_dir().join(format!("lingclaw-prompt-cache-{}", now_epoch()));
    let _ = std::fs::create_dir_all(&workspace);
    prompts::ensure_session_workspace(&workspace);
    let config = test_config();
    let disabled = std::collections::HashSet::new();
    let before = system_prompt_cache_metrics();

    let _ = build_system_prompt_with_query_cached(
        &config,
        &workspace,
        &config.model,
        &disabled,
        Some("review the performance optimization plan"),
    );
    let middle = system_prompt_cache_metrics();
    let _ = build_system_prompt_with_query_cached(
        &config,
        &workspace,
        &config.model,
        &disabled,
        Some("review the performance optimization plan"),
    );
    let after = system_prompt_cache_metrics();

    assert!(
        middle.1 >= before.1 + 1,
        "first render should miss the prompt cache"
    );
    assert!(
        after.0 >= middle.0 + 1,
        "second render should hit the prompt cache"
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn format_token_count_uses_k_and_m_units() {
    assert_eq!(format_token_count(999), "999");
    assert_eq!(format_token_count(1_200), "1.2K");
    assert_eq!(format_token_count(128_000), "128K");
    assert_eq!(format_token_count(1_250_000), "1.3M");
}

#[test]
fn build_session_usage_formats_totals() {
    let mut session = test_session("usage", "Usage", None);
    session.input_tokens = 12_300;
    session.output_tokens = 4_560;
    session.daily_input_tokens = 2_300;
    session.daily_output_tokens = 560;

    let usage = build_session_usage(&session);

    assert!(usage.contains("today_usage_est: # 当前会话今日 token 使用估算"));
    assert!(usage.contains("\tinput_tokens: 2.3K"));
    assert!(usage.contains("\toutput_tokens: 560"));
    assert!(usage.contains("total_usage_est: # 当前会话累计 token 使用估算"));
    assert!(usage.contains("\ttotal_tokens: 16.9K"));
    assert!(usage.contains("total_input_tokens: 12.3K"));
    assert!(usage.contains("total_output_tokens: 4.6K"));
}

#[test]
fn build_session_usage_resets_today_window_when_day_changes() {
    let mut session = test_session("usage-day", "Usage Day", None);
    session.input_tokens = 12_300;
    session.output_tokens = 4_560;
    session.daily_input_tokens = 2_300;
    session.daily_output_tokens = 560;
    session.token_usage_day = "1999-01-01".to_string();

    let usage = build_session_usage(&session);

    assert!(usage.contains("\tinput_tokens: 0"));
    assert!(usage.contains("\toutput_tokens: 0"));
    assert!(usage.contains("total_input_tokens: 12.3K"));
    assert!(usage.contains("total_output_tokens: 4.6K"));
}

#[test]
fn build_global_today_usage_sums_all_sessions() {
    let mut first = test_session("one", "One", None);
    first.daily_input_tokens = 2_300;
    first.daily_output_tokens = 560;

    let mut second = test_session("two", "Two", None);
    second.daily_input_tokens = 700;
    second.daily_output_tokens = 440;

    let mut third = test_session("three", "Three", None);
    third.daily_input_tokens = 999;
    third.daily_output_tokens = 1;
    third.token_usage_day = "1999-01-01".to_string();

    let sessions = HashMap::from([
        (first.id.clone(), first),
        (second.id.clone(), second),
        (third.id.clone(), third),
    ]);

    let usage = build_global_today_usage(sessions.values());

    assert!(usage.contains("global_today_usage_est: # 所有会话今日 token 使用估算"));
    assert!(usage.contains("input_tokens: 3K"));
    assert!(usage.contains("output_tokens: 1K"));
    assert!(usage.contains("total_tokens: 4K"));
}

#[test]
fn gather_global_today_usage_includes_unloaded_persisted_sessions() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let mut current = test_session(MAIN_SESSION_ID, "Main", None);
    current.daily_input_tokens = 2_300;
    current.daily_output_tokens = 560;

    {
        let mut sessions = rt.block_on(state.sessions.lock());
        sessions.insert(current.id.clone(), current);
    }

    let saved_id = format!("saved-usage-{}", now_epoch());
    let workspace = session_workspace_path(&saved_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let _guard = SavedSessionGuard {
        session_id: saved_id.clone(),
        workspace: workspace.clone(),
    };

    let mut saved = test_session(&saved_id, "Saved", None);
    saved.workspace = workspace.clone();
    saved.daily_input_tokens = 700;
    saved.daily_output_tokens = 440;
    rt.block_on(save_session_to_disk(&saved))
        .expect("saved session should persist");

    let usage = rt.block_on(gather_global_today_usage(&state));

    assert!(usage.contains("global_today_usage_est: # 所有会话今日 token 使用估算"));
    assert!(usage.contains("input_tokens: 3K"));
    assert!(usage.contains("output_tokens: 1K"));
    assert!(usage.contains("total_tokens: 4K"));
}

#[test]
fn build_usage_report_includes_session_and_global_sections() {
    let mut current = test_session("current", "Current", None);
    current.input_tokens = 12_300;
    current.output_tokens = 4_560;
    current.daily_input_tokens = 2_300;
    current.daily_output_tokens = 560;

    let report = build_usage_report(
        &current,
        "global_today_usage_est: # 所有会话今日 token 使用估算\n\tinput_tokens: 3K\n\toutput_tokens: 1K\n\ttotal_tokens: 4K",
    );

    assert!(report.contains("today_usage_est: # 当前会话今日 token 使用估算"));
    assert!(report.contains("total_usage_est: # 当前会话累计 token 使用估算"));
    assert!(report.contains("total_input_tokens: 12.3K"));
    assert!(report.contains("global_today_usage_est: # 所有会话今日 token 使用估算"));
    assert!(report.contains("\tinput_tokens: 3K"));
    assert!(report.contains("\toutput_tokens: 1K"));
    assert!(report.contains("\ttotal_tokens: 4K"));
}

#[test]
fn validate_session_id_rejects_trailing_dot_ids() {
    for id in ["foo.", "bar.."] {
        let err = crate::session_store::validate_session_id(id)
            .expect_err("trailing dot session id should be rejected");
        assert!(err.contains("Invalid session id"), "{id}: {err}");
    }
}

#[test]
fn validate_session_id_rejects_windows_reserved_device_names() {
    for id in ["con", "NUL", "prn.txt", "Com1", "LPT9.md"] {
        let err = crate::session_store::validate_session_id(id)
            .expect_err("windows reserved device name should be rejected");
        assert!(err.contains("Windows"), "{id}: {err}");
    }
}

#[test]
fn validate_session_id_rejects_reserved_top_level_config_dirs_case_insensitively() {
    for id in ["Skills", "SESSIONS", "System-Agents", "SYSTEM-SKILLS"] {
        let err = crate::session_store::validate_session_id(id)
            .expect_err("reserved config dir variant should be rejected");
        assert!(err.contains("reserved"), "{id}: {err}");
    }
}

#[test]
fn validate_session_id_rejects_reserved_top_level_config_dirs() {
    for id in [
        "agents",
        "memory",
        "sessions",
        "skills",
        "static",
        "system-agents",
        "system-skills",
    ] {
        let err = crate::session_store::validate_session_id(id)
            .expect_err("reserved config dir should be rejected");
        assert!(err.contains("reserved"), "{id}: {err}");
    }
}

#[test]
fn resolve_session_target_accepts_unique_prefix() {
    let known_ids = HashSet::from([
        "main".to_string(),
        "abc1234567890".to_string(),
        "def9999999999".to_string(),
    ]);

    let resolved = resolve_session_target("abc123", &known_ids).expect("prefix should resolve");

    assert_eq!(resolved, "abc1234567890");
}

#[test]
fn resolve_session_target_rejects_ambiguous_prefix() {
    let known_ids = HashSet::from(["abc1234567890".to_string(), "abc1239999999".to_string()]);

    let err = resolve_session_target("abc123", &known_ids).expect_err("prefix should be ambiguous");

    assert!(err.contains("ambiguous"));
}

#[test]
fn list_saved_session_ids_in_dir_uses_filenames_even_for_invalid_json() {
    let base = std::env::temp_dir().join(format!("lingclaw-test-{}", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");
    std::fs::write(base.join("good-session.json"), "not valid json")
        .expect("invalid json file should be created");
    std::fs::write(base.join("ignored.txt"), "ignore me").expect("non-json file should be created");

    let ids = list_saved_session_ids_in_dir(&base);

    assert!(ids.contains("good-session"));
    assert!(!ids.contains("ignored"));

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn list_saved_session_summaries_in_dir_includes_corrupt_files() {
    let base = std::env::temp_dir().join(format!("lingclaw-summary-test-{}", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");
    std::fs::write(base.join("broken-session.json"), "not valid json")
        .expect("invalid json file should be created");

    let summaries = list_saved_session_summaries_in_dir(&base);

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, "broken-session");
    assert!(summaries[0].corrupt);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn list_saved_session_summaries_in_dir_counts_messages_after_sanitization() {
    let session_id = format!("summary-sanitize-{}", now_epoch());
    let base = std::env::temp_dir().join(format!("lingclaw-summary-sanitize-test-{}", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");
    let payload = json!({
        "id": session_id,
        "name": "Sanitized Summary",
        "messages": [
            {
                "role": "system",
                "content": "system"
            },
            {
                "role": "assistant",
                "timestamp": 1
            }
        ],
        "created_at": 1,
        "updated_at": 1,
        "tool_calls_count": 0,
        "version": SESSION_VERSION
    });
    std::fs::write(
        base.join(format!("{session_id}.json")),
        serde_json::to_string_pretty(&payload).expect("payload should serialize"),
    )
    .expect("session file should be written");

    let summaries = list_saved_session_summaries_in_dir(&base);

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, session_id);
    assert_eq!(summaries[0].messages, 0);
    assert!(!summaries[0].corrupt);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn resolve_or_create_socket_session_honors_requested_session() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("legacy-session-{}", now_epoch());
    let session_workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&session_workspace).expect("workspace should be created");
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace: session_workspace,
    };
    let (tx, mut rx) = mpsc::channel::<String>(4);

    let connection_cancel = CancellationToken::new();
    let resolved = rt.block_on(resolve_or_create_socket_session(
        &state,
        &tx,
        Some(&session_id),
        1,
        &connection_cancel,
    ));

    assert_eq!(resolved, session_id);
    assert!(
        rt.block_on(state.sessions.lock())
            .contains_key(session_id.as_str())
    );

    let payloads = vec![
        rt.block_on(rx.recv())
            .expect("first payload should be sent"),
        rt.block_on(rx.recv())
            .expect("second payload should be sent"),
        rt.block_on(rx.recv())
            .expect("third payload should be sent"),
        rt.block_on(rx.recv())
            .expect("fourth payload should be sent"),
    ];
    let payload_types = payloads
        .iter()
        .map(|payload| {
            serde_json::from_str::<serde_json::Value>(payload)
                .expect("payload should be valid json")["type"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect::<Vec<_>>();
    assert!(payload_types.contains(&"session".to_string()));
    assert!(payload_types.contains(&"view_state".to_string()));
    assert!(payload_types.contains(&"todos_state".to_string()));
    assert!(payload_types.contains(&"history".to_string()));
}

#[test]
fn resolved_main_prefix_is_still_protected() {
    let known_ids = HashSet::from([MAIN_SESSION_ID.to_string(), "misc-session".to_string()]);

    let resolved = resolve_session_target("ma", &known_ids).expect("prefix should resolve");

    assert_eq!(resolved, MAIN_SESSION_ID);
}

#[test]
fn build_active_session_lines_lists_only_active_sessions_with_full_ids() {
    let config = test_config();
    let mut active_session = test_session(MAIN_SESSION_ID, "Main", None);
    active_session.input_token_source = "provider".to_string();
    active_session.output_token_source = "estimated".to_string();
    let sessions = HashMap::from([
        (MAIN_SESSION_ID.to_string(), active_session),
        (
            "idle-session-123".to_string(),
            test_session("idle-session-123", "Idle", Some("custom-model")),
        ),
    ]);
    let active_ids = HashSet::from([MAIN_SESSION_ID.to_string()]);

    let lines = build_active_session_lines(&sessions, &active_ids, &config);

    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains(MAIN_SESSION_ID));
    assert!(lines[0].contains("Main"));
    assert!(lines[0].contains("token_usage_source: in=provider out=estimated"));
    assert!(!lines[0].contains("Idle"));
}

#[test]
fn prune_messages_removes_complete_turns_without_recomputing_from_scratch() {
    let mut messages = vec![
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
            content: Some("a".repeat(500)),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("b".repeat(500)),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("keep".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    prune_messages(&mut messages, 50);

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].content.as_deref(), Some("keep"));
}

#[test]
fn sanitize_session_messages_removes_empty_assistant_reply() {
    let mut messages = vec![
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
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(1),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some(String::new()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "call-1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: "{}".into(),
                },
            }]),
            tool_call_id: None,
            timestamp: Some(2),
        },
    ];

    sanitize_session_messages(&mut messages);

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "assistant");
    assert!(messages[1].has_tool_calls());
}

#[test]
fn sanitize_session_messages_keeps_assistant_with_anthropic_thinking_blocks() {
    let mut messages = vec![
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
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: Some(vec![AnthropicThinkingBlock {
                block_type: "thinking".into(),
                thinking: Some("reasoning".into()),
                signature: Some("sig_123".into()),
                data: None,
            }]),
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(1),
        },
    ];

    sanitize_session_messages(&mut messages);

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].role, "assistant");
    assert!(messages[1].anthropic_thinking_blocks.is_some());
}

#[test]
fn sanitize_session_messages_drops_assistant_with_only_openai_responses_checkpoint() {
    let mut messages = vec![
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
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: Some(vec![AnthropicThinkingBlock {
                block_type: OPENAI_RESPONSES_RESPONSE_ID_BLOCK_TYPE.into(),
                thinking: None,
                signature: None,
                data: Some("resp_123".into()),
            }]),
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(1),
        },
    ];

    sanitize_session_messages(&mut messages);

    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, "system");
}

#[test]
fn load_session_from_disk_drops_empty_assistant_reply() {
    let session_id = format!("sanitize-load-{}", now_epoch());
    let path = sessions_dir().join(format!("{session_id}.json"));
    let payload = json!({
        "id": session_id,
        "name": "Test",
        "messages": [
            {
                "role": "system",
                "content": "system"
            },
            {
                "role": "assistant",
                "timestamp": 1773669433
            },
            {
                "role": "user",
                "content": "next"
            }
        ],
        "created_at": 1,
        "updated_at": 1,
        "tool_calls_count": 0,
        "think_level": "auto"
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&payload).expect("payload should serialize"),
    )
    .expect("session file should be written");

    let session = load_session_from_disk(
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .expect("session id should be valid"),
    )
    .expect("session should load");

    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].role, "system");
    assert_eq!(session.messages[1].role, "user");

    let _ = std::fs::remove_file(&path);
    let workspace = session_workspace_path(&session.id)
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn save_session_to_disk_omits_empty_assistant_reply_from_json() {
    let session_id = format!("sanitize-save-{}", now_epoch());
    let path = sessions_dir().join(format!("{session_id}.json"));
    let workspace = session_workspace_path(&session_id);
    let session = Session {
        id: session_id.clone(),
        name: "Test".into(),
        messages: vec![
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
                role: "assistant".into(),
                content: None,
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(1773669433),
            },
            ChatMessage {
                role: "user".into(),
                content: Some("next".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
        ],
        created_at: 1,
        updated_at: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: 0,
        workspace: workspace.clone(),
    };

    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    runtime
        .block_on(save_session_to_disk(&session))
        .expect("session should save");

    let data = std::fs::read_to_string(&path).expect("session file should be readable");
    let payload: serde_json::Value =
        serde_json::from_str(&data).expect("session file should contain valid json");
    let messages = payload["messages"]
        .as_array()
        .expect("messages should serialize as an array");

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    assert!(messages.iter().all(|message| {
        message["role"] != "assistant"
            || message
                .get("content")
                .and_then(|content| content.as_str())
                .is_some_and(|content| !content.is_empty())
            || message
                .get("tool_calls")
                .and_then(|tool_calls| tool_calls.as_array())
                .is_some_and(|tool_calls| !tool_calls.is_empty())
    }));

    let _ = std::fs::remove_file(&path);
    let session_dir = workspace
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(session_dir);
}

#[test]
fn save_session_to_disk_redacts_exec_arguments_in_messages_and_snapshots() {
    let session_id = format!("redact-exec-save-{}", now_epoch());
    let path = sessions_dir().join(format!("{session_id}.json"));
    let workspace = session_workspace_path(&session_id);
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace: workspace.clone(),
    };
    let session = Session {
        id: session_id,
        name: "Test".into(),
        messages: vec![
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
                role: "assistant".into(),
                content: None,
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![ToolCall {
                    id: "exec_call_1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "exec".into(),
                        arguments: r#"{"command":"curl -H \"Authorization: Bearer super-secret\" --api-key \"key-123\"","apiKey":"hook-secret"}"#.into(),
                    },
                }]),
                tool_call_id: None,
                timestamp: Some(1000),
            },
            ChatMessage {
                role: "assistant".into(),
                content: None,
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![ToolCall {
                    id: "task_call_1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "task".into(),
                        arguments: r#"{"agent":"reviewer","prompt":"Inspect logs"}"#.into(),
                    },
                }]),
                tool_call_id: None,
                timestamp: Some(1001),
            },
            ChatMessage {
                role: "tool".into(),
                content: Some("Found the issue in the logs.".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some("task_call_1".into()),
                timestamp: Some(1002),
            },
        ],
        created_at: 1,
        updated_at: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::from([(
            subagent_snapshot_storage_key("task_call_1", 1),
            SubagentHistorySnapshot {
                tools: vec![SubagentToolHistorySnapshot {
                    id: "tool-1".into(),
                    name: "exec".into(),
                    arguments: Some(
                        r#"{"command":"curl -H \"Authorization: Bearer nested-secret\"","access_token":"token-456"}"#
                            .into(),
                    ),
                    result: Some("ok".into()),
                    is_error: false,
                    duration_ms: 12,
                }],
                success: true,
                result_excerpt: Some("Found the issue in the logs.".into()),
                ..Default::default()
            },
        )]),
        todos: crate::todos::TodoSnapshot::default(),
        version: 0,
        workspace,
    };

    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    runtime
        .block_on(save_session_to_disk(&session))
        .expect("session should save");

    let data = std::fs::read_to_string(&path).expect("session file should be readable");
    let payload: serde_json::Value =
        serde_json::from_str(&data).expect("session file should contain valid json");
    let serialized = payload.to_string();

    assert!(serialized.contains("[REDACTED]"));
    assert!(!serialized.contains("super-secret"));
    assert!(!serialized.contains("key-123"));
    assert!(!serialized.contains("hook-secret"));
    assert!(!serialized.contains("nested-secret"));
    assert!(!serialized.contains("token-456"));
}

#[test]
fn save_session_to_disk_overwrites_existing_file() {
    let session_id = format!("overwrite-save-{}", now_epoch());
    let path = sessions_dir().join(format!("{session_id}.json"));
    let workspace = session_workspace_path(&session_id);
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");

    let first = Session {
        id: session_id.clone(),
        name: "First".into(),
        messages: vec![ChatMessage {
            role: "system".into(),
            content: Some("first".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        }],
        created_at: 1,
        updated_at: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: 1,
        workspace: workspace.clone(),
    };
    runtime
        .block_on(save_session_to_disk(&first))
        .expect("first save should succeed");

    let second = Session {
        name: "Second".into(),
        updated_at: 2,
        ..first.clone()
    };
    runtime
        .block_on(save_session_to_disk(&second))
        .expect("second save should overwrite existing file");

    let data = std::fs::read_to_string(&path).expect("session file should be readable");
    let payload: serde_json::Value =
        serde_json::from_str(&data).expect("session file should contain valid json");
    assert_eq!(payload["name"], "Second");
    assert_eq!(payload["updated_at"], 2);

    let _ = std::fs::remove_file(&path);
    let session_dir = workspace
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(session_dir);
}

#[test]
fn save_session_to_disk_skips_identical_payload_rewrite() {
    let session_id = format!("skip-identical-save-{}", now_epoch());
    let path = sessions_dir().join(format!("{session_id}.json"));
    let workspace = session_workspace_path(&session_id);
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace: workspace.clone(),
    };
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");

    let session = Session {
        id: session_id,
        name: "Stable".into(),
        messages: vec![
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
                content: Some("hello".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(1),
            },
        ],
        created_at: 1,
        updated_at: 1,
        tool_calls_count: 0,
        input_tokens: 12,
        output_tokens: 34,
        daily_input_tokens: 12,
        daily_output_tokens: 34,
        input_token_source: default_token_usage_source(),
        output_token_source: default_token_usage_source(),
        token_usage_day: prompts::current_local_snapshot().today(),
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: default_think_level(),
        show_react: true,
        show_tools: true,
        show_reasoning: true,
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace,
    };

    runtime
        .block_on(save_session_to_disk(&session))
        .expect("first save should succeed");
    let first_modified = std::fs::metadata(&path)
        .expect("session file should exist")
        .modified()
        .expect("session file should have modified time");

    std::thread::sleep(std::time::Duration::from_millis(1100));

    runtime
        .block_on(save_session_to_disk(&session))
        .expect("second identical save should succeed");
    let second_modified = std::fs::metadata(&path)
        .expect("session file should still exist")
        .modified()
        .expect("session file should have modified time");

    assert_eq!(
        first_modified, second_modified,
        "identical persisted payload should not rewrite the session file"
    );
}

#[test]
fn load_session_from_disk_trims_incomplete_tool_transaction() {
    let session_id = format!("trim-load-{}", now_epoch());
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace: session_workspace_path(&session_id),
    };
    let path = sessions_dir().join(format!("{session_id}.json"));
    let payload = json!({
        "id": session_id,
        "name": "TrimLoad",
        "messages": [
            {
                "role": "system",
                "content": "system"
            },
            {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "exec",
                            "arguments": "{\"command\":\"echo hi\"}"
                        }
                    },
                    {
                        "id": "call_2",
                        "type": "function",
                        "function": {
                            "name": "exec",
                            "arguments": "{\"command\":\"echo bye\"}"
                        }
                    }
                ]
            },
            {
                "role": "tool",
                "content": "hi",
                "tool_call_id": "call_1"
            },
            {
                "role": "user",
                "content": "after"
            }
        ],
        "created_at": 1,
        "updated_at": 1,
        "tool_calls_count": 1,
        "version": 1
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&payload).expect("payload should serialize"),
    )
    .expect("session file should be written");

    let session = load_session_from_disk(
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .expect("session id should be valid"),
    )
    .expect("session should load");

    assert_eq!(session.messages.len(), 1);
    assert_eq!(session.messages[0].role, "system");

    let _ = std::fs::remove_file(&path);
    let workspace = session_workspace_path(&session.id)
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn resolve_path_clamps_parent_escape_attempts() {
    let base = std::env::temp_dir().join(format!("lingclaw-resolve-{}", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");

    let resolved = resolve_path("../../outside.txt", &base);

    assert_eq!(resolved, base.canonicalize().unwrap_or(base.clone()));

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn resolve_path_checked_rejects_absolute_paths_outside_workspace() {
    let base = std::env::temp_dir().join(format!("lingclaw-resolve-check-{}", now_epoch()));
    let outside = std::env::temp_dir().join(format!("lingclaw-outside-{}.txt", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");

    let message = resolve_path_checked(&outside.to_string_lossy(), &base)
        .expect_err("outside path should be rejected");

    assert!(message.contains("outside the session workspace"));

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn resolve_path_checked_allows_workspace_root_absolute_path() {
    let base = std::env::temp_dir().join(format!("lingclaw-resolve-root-{}", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");

    let resolved = resolve_path_checked(&base.to_string_lossy(), &base)
        .expect("workspace root path should be allowed");

    assert_eq!(resolved, base.canonicalize().unwrap_or(base.clone()));

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn resolve_path_checked_allows_relative_path_that_normalizes_to_workspace_root() {
    let base = std::env::temp_dir().join(format!("lingclaw-resolve-normalized-{}", now_epoch()));
    let nested = base.join("nested");
    std::fs::create_dir_all(&nested).expect("nested dir should be created");

    let resolved = resolve_path_checked("nested/..", &base)
        .expect("normalized in-workspace path should be allowed");

    assert_eq!(resolved, base.canonicalize().unwrap_or(base.clone()));

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn resolve_path_checked_rejects_bootstrap_baseline_dir() {
    let base = std::env::temp_dir().join(format!("lingclaw-resolve-bootstrap-{}", now_epoch()));
    let bootstrap_dir = base.join(".lingclaw-bootstrap");
    std::fs::create_dir_all(&bootstrap_dir).expect("bootstrap dir should be created");

    let message = resolve_path_checked(".lingclaw-bootstrap/IDENTITY.md", &base)
        .expect_err("bootstrap baseline dir should be protected");

    assert!(message.contains("protected internal workspace data"));

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn read_file_reports_workspace_escape_clearly() {
    let base = std::env::temp_dir().join(format!("lingclaw-read-file-{}", now_epoch()));
    let outside = std::env::temp_dir().join(format!("lingclaw-outside-read-{}.txt", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");
    std::fs::write(&outside, "outside").expect("outside file should be written");

    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    let result = runtime.block_on(tools::fs::tool_read_file(
        &json!({ "path": outside.to_string_lossy().to_string() }),
        &test_config(),
        &base,
    ));

    assert!(result.contains("read_file error: path '"));
    assert!(result.contains("outside the session workspace"));

    let _ = std::fs::remove_file(&outside);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn generate_shutdown_token_returns_64_hex_chars() {
    let token = generate_shutdown_token().expect("secure shutdown token should be generated");

    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|ch| ch.is_ascii_hexdigit()));
}

#[tokio::test]
async fn api_client_config_returns_upload_token() {
    let state = Arc::new(test_app_state());
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let Json(payload) = api_client_config(headers, State(state.clone()))
        .await
        .expect("local request should be accepted");

    assert_eq!(payload["upload_token"], state.upload_token);
}

#[tokio::test]
async fn api_sessions_lists_loaded_non_main_sessions() {
    let state = Arc::new(test_app_state());
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(
            MAIN_SESSION_ID.to_string(),
            test_session(MAIN_SESSION_ID, "Main", None),
        );
        sessions.insert(
            "verify-open".to_string(),
            test_session("verify-open", "verify-open", None),
        );
    }

    let response = api_sessions(State(state)).await.into_response();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should be readable");
    let payload: serde_json::Value =
        serde_json::from_slice(&body).expect("payload should be valid json");
    let sessions = payload["sessions"]
        .as_array()
        .expect("sessions array should be present");

    assert!(
        sessions
            .iter()
            .any(|session| session["id"] == MAIN_SESSION_ID)
    );
    assert!(
        sessions
            .iter()
            .any(|session| session["id"] == "verify-open")
    );
}

#[tokio::test]
async fn api_sessions_includes_corrupt_persisted_sessions() {
    let session_id = format!(
        "api-sessions-corrupt-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    );
    let session_file = sessions_dir().join(format!("{session_id}.json"));
    tokio::fs::write(&session_file, b"not valid json")
        .await
        .expect("corrupt session file should be written");

    let state = Arc::new(test_app_state());

    let response = api_sessions(State(state)).await.into_response();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should be readable");
    let payload: serde_json::Value =
        serde_json::from_slice(&body).expect("payload should be valid json");
    let sessions = payload["sessions"]
        .as_array()
        .expect("sessions array should be present");

    let corrupt_session = sessions
        .iter()
        .find(|session| session["id"] == session_id)
        .expect("corrupt persisted session should be listed");
    assert_eq!(corrupt_session["corrupt"], true);
    assert_eq!(corrupt_session["name"], "[Corrupt Session]");

    let _ = tokio::fs::remove_file(&session_file).await;
}

#[tokio::test]
async fn api_session_skills_defaults_system_skills_to_disabled() {
    let state = Arc::new(test_app_state());
    let session_id = format!("skills-default-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace: workspace.clone(),
    };

    let mut session = test_session(&session_id, "Skills Default", None);
    session.workspace = workspace;
    session.version = SESSION_VERSION;
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let Json(payload) = api_session_skills(
        Query(SessionQuery {
            session: Some(session_id),
        }),
        headers,
        State(state),
    )
    .await
    .expect("session skills should load");

    let skills = payload["skills"]
        .as_array()
        .expect("skills should be an array");
    assert!(!skills.is_empty(), "system skills should be discovered");
    assert!(skills.iter().all(|skill| skill["enabled"] == false));
    assert_eq!(payload["enabledSystemSkills"], json!([]));
    let disabled = payload["disabledSystemSkills"]
        .as_array()
        .expect("disabled list should be present");
    assert_eq!(disabled.len(), skills.len());
}

#[tokio::test]
async fn api_put_session_skills_persists_enabled_set_and_refreshes_prompt() {
    let state = Arc::new(test_app_state());
    let session_id = format!("skills-put-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace: workspace.clone(),
    };

    let mut session = test_session(&session_id, "Skills Put", None);
    session.workspace = workspace;
    session.version = SESSION_VERSION;
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let Json(payload) = api_put_session_skills(
        Query(SessionQuery {
            session: Some(session_id.clone()),
        }),
        headers,
        State(state.clone()),
        Json(SessionSkillsUpdateRequest {
            enabled_system_skills: vec!["anthropics/pdf".to_string()],
            known_system_skills: None,
        }),
    )
    .await
    .expect("session skills should save");

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["enabledSystemSkills"], json!(["anthropics/pdf"]));

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&session_id)
        .expect("session should remain loaded");
    assert!(session.enabled_system_skills.contains("anthropics/pdf"));
    assert!(!session.enabled_system_skills.contains("anthropics/xlsx"));
    let system_prompt = session.messages[0]
        .content
        .as_deref()
        .expect("system prompt should be refreshed");
    assert!(system_prompt.contains("system://skills/anthropics/pdf/SKILL.md"));
    assert!(!system_prompt.contains("system://skills/anthropics/xlsx/SKILL.md"));
    drop(sessions);

    let persisted = load_session_from_disk(&session_id).expect("session should persist");
    assert!(persisted.enabled_system_skills.contains("anthropics/pdf"));
    assert!(persisted.disabled_system_skills.is_empty());
}

#[tokio::test]
async fn api_put_session_skills_only_updates_client_known_skill_ids() {
    let state = Arc::new(test_app_state());
    let session_id = format!("skills-known-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    prompts::init_session_prompt_files(&workspace);
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace: workspace.clone(),
    };

    let mut session = test_session(&session_id, "Skills Known", None);
    session.workspace = workspace;
    session.version = SESSION_VERSION;
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let Json(payload) = api_put_session_skills(
        Query(SessionQuery {
            session: Some(session_id.clone()),
        }),
        headers,
        State(state.clone()),
        Json(SessionSkillsUpdateRequest {
            enabled_system_skills: vec!["anthropics/pdf".to_string()],
            known_system_skills: Some(vec!["anthropics/pdf".to_string()]),
        }),
    )
    .await
    .expect("session skills should save");

    assert_eq!(payload["ok"], true);
    assert_eq!(payload["enabledSystemSkills"], json!(["anthropics/pdf"]));

    let sessions = state.sessions.lock().await;
    let session = sessions
        .get(&session_id)
        .expect("session should remain loaded");
    assert!(
        !session.enabled_system_skills.contains("anthropics/xlsx"),
        "skills outside knownSystemSkills should keep their existing disabled state"
    );
    let system_prompt = session.messages[0]
        .content
        .as_deref()
        .expect("system prompt should be refreshed");
    assert!(system_prompt.contains("system://skills/anthropics/pdf/SKILL.md"));
    assert!(!system_prompt.contains("system://skills/anthropics/xlsx/SKILL.md"));
}

#[tokio::test]
async fn api_put_session_skills_rejects_unknown_skill_ids() {
    let state = Arc::new(test_app_state());
    let session_id = format!("skills-invalid-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace: workspace.clone(),
    };

    let mut session = test_session(&session_id, "Skills Invalid", None);
    session.workspace = workspace;
    session.version = SESSION_VERSION;
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let error = api_put_session_skills(
        Query(SessionQuery {
            session: Some(session_id),
        }),
        headers,
        State(state),
        Json(SessionSkillsUpdateRequest {
            enabled_system_skills: vec!["not-a-real-skill".to_string()],
            known_system_skills: None,
        }),
    )
    .await
    .expect_err("unknown skill id should be rejected");

    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert!(
        error.1["error"]
            .as_str()
            .is_some_and(|message| message.contains("Unknown system skill id"))
    );
}

#[tokio::test]
async fn api_session_skills_returns_not_found_for_unknown_sessions() {
    let state = Arc::new(test_app_state());
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let error = api_session_skills(
        Query(SessionQuery {
            session: Some(format!("missing-skills-{}", now_epoch())),
        }),
        headers,
        State(state),
    )
    .await
    .expect_err("missing session should be rejected");

    assert_eq!(error.0, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_session_skills_reflects_skills_system_command_state() {
    let state = test_app_state();
    let session_id = format!("skills-command-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace: workspace.clone(),
    };

    let mut session = test_session(&session_id, "Skills Command", None);
    session.workspace = workspace;
    session.version = SESSION_VERSION;
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/skills-system install anthropics/pdf",
        &session_id,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("skills-system command should resolve");
    assert_eq!(result.response_type, "system");

    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));
    let Json(payload) = api_session_skills(
        Query(SessionQuery {
            session: Some(session_id),
        }),
        headers,
        State(Arc::new(state)),
    )
    .await
    .expect("session skills should load");

    let skills = payload["skills"]
        .as_array()
        .expect("skills should be an array");
    let pdf = skills
        .iter()
        .find(|skill| skill["id"] == "anthropics/pdf")
        .expect("pdf skill should be listed");
    assert_eq!(pdf["enabled"], true);
}

#[tokio::test]
async fn api_session_skills_expands_enabled_group_patterns_for_round_trip() {
    let state = Arc::new(test_app_state());
    let session_id = format!("skills-group-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace: workspace.clone(),
    };

    let mut session = test_session(&session_id, "Skills Group", None);
    session.workspace = workspace;
    session.version = SESSION_VERSION;
    state
        .sessions
        .lock()
        .await
        .insert(session_id.clone(), session);

    let (tx, _rx) = tokio::sync::mpsc::channel::<String>(4);
    let result = handle_command(
        "/skills-system install anthropics",
        &session_id,
        1,
        &state,
        &tx,
        &CancellationToken::new(),
    )
    .await
    .expect("skills-system command should resolve");
    assert_eq!(result.response_type, "system");

    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));
    let Json(payload) = api_session_skills(
        Query(SessionQuery {
            session: Some(session_id.clone()),
        }),
        headers.clone(),
        State(state.clone()),
    )
    .await
    .expect("session skills should load");

    let enabled = payload["enabledSystemSkills"]
        .as_array()
        .expect("enabled list should be present");
    assert!(
        enabled.iter().all(|id| id != "anthropics"),
        "GET should expose concrete skill ids, not persisted group patterns"
    );
    assert!(enabled.iter().any(|id| id == "anthropics/pdf"));

    let Json(round_trip_payload) = api_put_session_skills(
        Query(SessionQuery {
            session: Some(session_id),
        }),
        headers,
        State(state),
        Json(SessionSkillsUpdateRequest {
            enabled_system_skills: enabled
                .iter()
                .filter_map(|id| id.as_str().map(str::to_string))
                .collect(),
            known_system_skills: None,
        }),
    )
    .await
    .expect("expanded enabled ids should be accepted by PUT");
    assert_eq!(round_trip_payload["ok"], true);
}

#[tokio::test]
async fn api_usage_loads_persisted_session_not_yet_in_memory() {
    let state = Arc::new(test_app_state());
    let session_id = format!("usage-persisted-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace,
    };

    let mut persisted_session = test_session(&session_id, "Persisted Usage", None);
    persisted_session.workspace = session_workspace_path(&session_id);
    persisted_session.version = SESSION_VERSION;
    persisted_session.input_tokens = 77;
    persisted_session.output_tokens = 11;
    persisted_session.input_token_source = "provider".to_string();
    persisted_session.output_token_source = "estimated".to_string();
    save_session_to_disk(&persisted_session)
        .await
        .expect("session should persist to disk");

    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let Json(payload) = api_usage(
        Query(SessionQuery {
            session: Some(session_id.clone()),
        }),
        headers,
        State(state.clone()),
    )
    .await
    .expect("local request should be accepted");

    assert_eq!(payload["total_input"], 77);
    assert_eq!(payload["total_output"], 11);
    assert_eq!(payload["input_source"], "provider");
    assert_eq!(payload["output_source"], "estimated");
    assert!(
        state
            .sessions
            .lock()
            .await
            .contains_key(session_id.as_str())
    );
}

#[tokio::test]
async fn api_usage_uses_requested_session_query() {
    let state = Arc::new(test_app_state());
    let mut main_session = test_session(MAIN_SESSION_ID, "Main", None);
    main_session.input_tokens = 100;
    main_session.output_tokens = 20;

    let mut alt_session = test_session("usage-alt", "Usage Alt", None);
    alt_session.input_tokens = 55;
    alt_session.output_tokens = 5;
    alt_session.input_token_source = "provider".to_string();
    alt_session.output_token_source = "estimated".to_string();

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(main_session.id.clone(), main_session);
        sessions.insert(alt_session.id.clone(), alt_session);
    }

    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let Json(payload) = api_usage(
        Query(SessionQuery {
            session: Some("usage-alt".to_string()),
        }),
        headers,
        State(state),
    )
    .await
    .expect("local request should be accepted");

    assert_eq!(payload["total_input"], 55);
    assert_eq!(payload["total_output"], 5);
    assert_eq!(payload["input_source"], "provider");
    assert_eq!(payload["output_source"], "estimated");
}

#[tokio::test]
async fn api_usage_returns_token_sources() {
    let state = Arc::new(test_app_state());
    let mut session = test_session(MAIN_SESSION_ID, "Main", None);
    session.input_tokens = 123;
    session.output_tokens = 45;
    session.daily_input_tokens = 12;
    session.daily_output_tokens = 3;
    session.input_token_source = "provider".to_string();
    session.output_token_source = "estimated".to_string();

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session.id.clone(), session);
    }

    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let Json(payload) = api_usage(Query(SessionQuery { session: None }), headers, State(state))
        .await
        .expect("local request should be accepted");

    assert_eq!(payload["input_source"], "provider");
    assert_eq!(payload["output_source"], "estimated");
    assert_eq!(payload["source_scope"], "latest_update");
    assert_eq!(payload["total"], 168);
}

#[tokio::test]
async fn api_usage_rolls_over_stale_daily_usage_before_serializing() {
    let state = Arc::new(test_app_state());
    let mut session = test_session(MAIN_SESSION_ID, "Main", None);
    let yesterday = (chrono::Local::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let mut providers = HashMap::new();
    providers.insert("openai".to_string(), [12, 3]);
    session.token_usage_day = yesterday.clone();
    session.daily_input_tokens = 12;
    session.daily_output_tokens = 3;
    session.daily_provider_usage = providers;

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session.id.clone(), session);
    }

    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let Json(payload) = api_usage(
        Query(SessionQuery { session: None }),
        headers,
        State(state.clone()),
    )
    .await
    .expect("local request should be accepted");

    assert_eq!(payload["daily_input"], 0);
    assert_eq!(payload["daily_output"], 0);
    assert_eq!(payload["daily_providers"], json!({}));
    assert_eq!(payload["daily_roles"], json!({}));
    assert_eq!(
        payload["usage_history"],
        json!([{
            "date": yesterday,
            "input": 12,
            "output": 3,
            "providers": {
                "openai": [12, 3]
            },
            "roles": {}
        }])
    );

    let persisted = state
        .sessions
        .lock()
        .await
        .get(MAIN_SESSION_ID)
        .cloned()
        .expect("session should still exist");
    assert_eq!(persisted.daily_input_tokens, 0);
    assert_eq!(persisted.daily_output_tokens, 0);
    assert!(persisted.daily_provider_usage.is_empty());
    assert_eq!(persisted.usage_history.len(), 1);
}

#[tokio::test]
async fn api_test_model_rejects_placeholder_requests_without_saved_provider_context() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let state = Arc::new(test_app_state());
    let result = api_test_model(
        headers,
        State(state),
        Json(json!({
            "baseUrl": "${LINGCLAW_TEST_UNSET_BASE_URL_DO_NOT_SET}",
            "apiKey": "${LINGCLAW_TEST_UNSET_API_KEY_DO_NOT_SET}",
            "api": "openai-completions",
            "modelId": "gpt-4o-mini"
        })),
    )
    .await;

    let (status, body) = result.expect_err("missing placeholder env should fail validation");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body.0["error"].as_str(),
        Some("Save config before testing providers that use ${ENV} placeholders.")
    );
}

#[tokio::test]
async fn api_put_config_rejects_invalid_provider_names() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let state = Arc::new(test_app_state());
    let result = api_put_config(
        headers,
        State(state),
        Json(json!({
            "config": {
                "models": {
                    "providers": {
                        "openai/test": {
                            "api": "openai-completions",
                            "baseUrl": "https://api.openai.com/v1",
                            "apiKey": "key",
                            "models": []
                        }
                    }
                }
            }
        })),
    )
    .await;

    let (status, Json(body)) = result.expect_err("invalid provider names should fail");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|msg| msg.contains("cannot contain '/'"))
    );
}

#[tokio::test]
async fn api_put_config_rejects_unknown_agent_provider_alias() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let state = Arc::new(test_app_state());
    let result = api_put_config(
        headers,
        State(state),
        Json(json!({
            "config": {
                "models": {
                    "providers": {
                        "openai-work": {
                            "api": "openai-completions",
                            "baseUrl": "https://gateway.example/v1",
                            "apiKey": "key",
                            "models": [
                                {
                                    "id": "gpt-4o-mini"
                                }
                            ]
                        }
                    }
                },
                "agents": {
                    "defaults": {
                        "model": {
                            "primary": "missing/gpt-4o-mini"
                        }
                    }
                }
            }
        })),
    )
    .await;

    let (status, Json(body)) = result.expect_err("unknown agent provider aliases should fail");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|msg| msg.contains("agents.defaults.model.primary"))
    );
}

#[tokio::test]
async fn api_put_config_rejects_unknown_agent_provider_prefix_without_models_config() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let state = Arc::new(test_app_state());
    let result = api_put_config(
        headers,
        State(state),
        Json(json!({
            "config": {
                "agents": {
                    "defaults": {
                        "model": {
                            "primary": "missing/gpt-4o-mini"
                        }
                    }
                }
            }
        })),
    )
    .await;

    let (status, Json(body)) =
        result.expect_err("unknown provider prefixes should fail without models config");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|msg| msg.contains("agents.defaults.model.primary"))
    );
}

#[tokio::test]
async fn api_put_config_rejects_unknown_agent_model_id_for_configured_provider() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let state = Arc::new(test_app_state());
    let result = api_put_config(
        headers,
        State(state),
        Json(json!({
            "config": {
                "models": {
                    "providers": {
                        "openai-work": {
                            "api": "openai-completions",
                            "baseUrl": "https://gateway.example/v1",
                            "apiKey": "key",
                            "models": [
                                {
                                    "id": "gpt-4o-mini"
                                }
                            ]
                        }
                    }
                },
                "agents": {
                    "defaults": {
                        "model": {
                            "primary": "openai-work/typo-model"
                        }
                    }
                }
            }
        })),
    )
    .await;

    let (status, Json(body)) = result.expect_err("unknown configured model ids should fail");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|msg| msg.contains("unknown model 'typo-model'"))
    );
}

#[tokio::test]
async fn api_put_config_rejects_empty_mcp_command() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let state = Arc::new(test_app_state());
    let result = api_put_config(
        headers,
        State(state),
        Json(json!({
            "config": {
                "mcpServers": {
                    "empty-command": {
                        "command": "",
                        "args": []
                    }
                }
            }
        })),
    )
    .await;

    let (status, Json(body)) = result.expect_err("empty MCP command should fail");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|msg| msg.contains("mcpServers.empty-command"))
    );
}

#[tokio::test]
async fn api_put_config_rejects_invalid_provider_api_kind() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let state = Arc::new(test_app_state());
    let result = api_put_config(
        headers,
        State(state),
        Json(json!({
            "config": {
                "models": {
                    "providers": {
                        "openai-work": {
                            "api": "anthorpic",
                            "baseUrl": "https://gateway.example/v1",
                            "apiKey": "key",
                            "models": []
                        }
                    }
                }
            }
        })),
    )
    .await;

    let (status, Json(body)) = result.expect_err("invalid provider api kinds should fail");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|msg| msg.contains("unsupported api 'anthorpic'"))
    );
}

#[tokio::test]
async fn api_put_config_rejects_zero_mcp_timeout() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let state = Arc::new(test_app_state());
    let result = api_put_config(
        headers,
        State(state),
        Json(json!({
            "config": {
                "mcpServers": {
                    "zero-timeout": {
                        "command": "uvx",
                        "args": [],
                        "timeoutSecs": 0
                    }
                }
            }
        })),
    )
    .await;

    let (status, Json(body)) = result.expect_err("zero MCP timeout should fail");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|msg| msg.contains("greater than 0"))
    );
}

#[tokio::test]
async fn api_put_config_rejects_mcp_cwd_outside_workspace() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let state = Arc::new(test_app_state());
    let result = api_put_config(
        headers,
        State(state),
        Json(json!({
            "config": {
                "mcpServers": {
                    "outside-workspace": {
                        "command": "uvx",
                        "args": [],
                        "cwd": "../outside"
                    }
                }
            }
        })),
    )
    .await;

    let (status, Json(body)) = result.expect_err("MCP cwd escaping the workspace should fail");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().is_some_and(|msg| {
        msg.contains("mcpServers.outside-workspace.cwd")
            && msg.contains("outside the session workspace")
    }));
}

#[tokio::test]
async fn api_put_config_rejects_empty_provider_model_id() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));

    let state = Arc::new(test_app_state());
    let result = api_put_config(
        headers,
        State(state),
        Json(json!({
            "config": {
                "models": {
                    "providers": {
                        "openai-work": {
                            "api": "openai-completions",
                            "baseUrl": "https://gateway.example/v1",
                            "apiKey": "key",
                            "models": [
                                {
                                    "id": ""
                                }
                            ]
                        }
                    }
                }
            }
        })),
    )
    .await;

    let (status, Json(body)) = result.expect_err("empty provider model ids should fail");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]
            .as_str()
            .is_some_and(|msg| msg.contains("model id cannot be empty"))
    );
}

#[tokio::test]
async fn read_config_file_snapshot_waits_for_active_writer() {
    let base = std::env::temp_dir().join(format!("lingclaw-config-read-{}", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");
    let path = base.join("config.json");
    std::fs::write(&path, "{\"ok\":true}").expect("config file should be written");

    let write_guard = CONFIG_FILE_LOCK.write().await;
    let task = tokio::spawn({
        let path = path.clone();
        async move {
            read_config_file_snapshot(&path)
                .await
                .expect("config read should succeed")
        }
    });

    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
    assert!(
        !task.is_finished(),
        "reader should wait for active config writer"
    );

    drop(write_guard);

    let content = task.await.expect("reader task should join");
    assert_eq!(content, "{\"ok\":true}");

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn validate_local_request_headers_accepts_loopback_host_and_origin() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("localhost:18989"));
    headers.insert("origin", HeaderValue::from_static("http://127.0.0.1:18989"));

    assert!(validate_local_request_headers(&headers).is_ok());
}

#[test]
fn validate_local_request_headers_rejects_non_local_host() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("evil.example"));

    let err = validate_local_request_headers(&headers).expect_err("remote host must be rejected");

    assert_eq!(err.0, StatusCode::FORBIDDEN);
    assert_eq!(
        err.1.0["error"],
        "Blocked non-local request: Host header must target localhost or a loopback address"
    );
}

#[test]
fn validate_local_request_headers_rejects_non_local_origin() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("127.0.0.1:18989"));
    headers.insert("origin", HeaderValue::from_static("https://evil.example"));

    let err = validate_local_request_headers(&headers)
        .expect_err("remote origin must be rejected even for loopback host");

    assert_eq!(err.0, StatusCode::FORBIDDEN);
    assert_eq!(
        err.1.0["error"],
        "Blocked non-local request: Origin/Referer must be localhost or a loopback address"
    );
}

#[tokio::test]
async fn api_client_config_rejects_non_local_host() {
    let state = Arc::new(test_app_state());
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("evil.example"));

    let err = api_client_config(headers, State(state))
        .await
        .expect_err("remote host should not receive upload token");

    assert_eq!(err.0, StatusCode::FORBIDDEN);
}

#[test]
fn find_static_dir_from_prefers_exe_ancestors() {
    let base = std::env::temp_dir().join(format!("lingclaw-static-exe-{}", now_epoch()));
    let exe_dir = base.join("bin");
    let static_dir = base.join("static");
    std::fs::create_dir_all(&exe_dir).expect("bin dir should be created");
    std::fs::create_dir_all(&static_dir).expect("static dir should be created");
    std::fs::write(static_dir.join("index.html"), "ok").expect("index should be written");

    let resolved = find_static_dir_from(Some(&exe_dir.join("lingclaw.exe")), None);

    assert_eq!(resolved, static_dir);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn find_static_dir_from_falls_back_to_cwd() {
    let base = std::env::temp_dir().join(format!("lingclaw-static-cwd-{}", now_epoch()));
    let static_dir = base.join("static");
    std::fs::create_dir_all(&static_dir).expect("static dir should be created");
    std::fs::write(static_dir.join("index.html"), "ok").expect("index should be written");

    let resolved = find_static_dir_from(None, Some(&base));

    assert_eq!(resolved, static_dir);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn find_static_dir_from_does_not_walk_past_expected_exe_ancestors() {
    let base = std::env::temp_dir().join(format!("lingclaw-static-boundary-{}", now_epoch()));
    let outer_static = base.join("outer").join("static");
    let exe_dir = base
        .join("outer")
        .join("project")
        .join("target")
        .join("debug");
    std::fs::create_dir_all(&outer_static).expect("outer static dir should be created");
    std::fs::create_dir_all(&exe_dir).expect("exe dir should be created");
    std::fs::write(outer_static.join("index.html"), "wrong").expect("outer index should exist");

    let resolved = find_static_dir_from(Some(&exe_dir.join("lingclaw.exe")), None);

    assert_eq!(resolved, PathBuf::from("static"));

    let _ = std::fs::remove_dir_all(&base);
}

// ── Phase 2: observation summary + history payload integration tests ─────

#[test]
fn observation_summary_does_not_appear_in_persisted_tool_result() {
    // Verify that even with large tool results, the session stores raw content
    // and no observation annotation leaks into the history payload.
    let big_result = format!("{{\"data\":\"{}\"}}", "y".repeat(6000));
    let session = Session {
        id: "obs-test".into(),
        name: "ObsTest".into(),
        messages: vec![
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
                role: "assistant".into(),
                content: Some(String::new()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_obs".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "exec".into(),
                        arguments: r#"{"command":"ls"}"#.into(),
                    },
                }]),
                tool_call_id: None,
                timestamp: Some(100),
            },
            ChatMessage {
                role: "tool".into(),
                content: Some(big_result.clone()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some("call_obs".into()),
                timestamp: Some(101),
            },
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: 0,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let msgs = payload["messages"].as_array().unwrap();
    let tool_entry = msgs.iter().find(|m| m["role"] == "tool_result").unwrap();
    let result_str = tool_entry["result"].as_str().unwrap();

    // Must be exact raw content —no "[Observation:" prefix
    assert_eq!(result_str, big_result.as_str());
    assert!(!result_str.starts_with("[Observation:"));
}

#[test]
fn observation_summaries_are_independent_of_session_messages() {
    // summarize_observations produces summaries from ToolResultEntry, not from session
    let entries = vec![
        agent::ToolResultEntry {
            id: "c1".into(),
            name: "exec".into(),
            result: "short".into(),
            duration_ms: 0,
            is_error: false,
            call_summary: None,
            trace: None,
        },
        agent::ToolResultEntry {
            id: "c2".into(),
            name: "read_file".into(),
            result: "z\n".repeat(3000),
            duration_ms: 0,
            is_error: false,
            call_summary: None,
            trace: None,
        },
    ];

    let summaries = agent::summarize_observations(&entries);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].tool_call_id, "c2");

    let hint = agent::build_observation_context_hint(&summaries, 0);
    assert!(hint.is_some());
    let hint_text = hint.unwrap();
    assert!(hint_text.contains("read_file"));
    assert!(hint_text.contains("3000 lines"));
}

#[test]
fn system_prompt_with_observation_hint_preserves_original_content() {
    // Simulate the pattern used in Analyze phase: appending hint to system prompt
    let mut msg = ChatMessage {
        role: "system".into(),
        content: Some("You are an assistant.".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };

    let summaries = vec![agent::ObservationSummary {
        tool_call_id: "c1".into(),
        tool_name: "exec".into(),
        byte_size: 8000,
        line_count: 200,
        hint: "exec returned 200 lines / 8000 bytes —focus on key findings".into(),
    }];
    if let Some(hint) = agent::build_observation_context_hint(&summaries, 0)
        && let Some(ref mut content) = msg.content
    {
        content.push_str("\n\n");
        content.push_str(&hint);
    }

    let content = msg.content.as_deref().unwrap();
    assert!(content.starts_with("You are an assistant."));
    assert!(content.contains("## Recent Observation Notes"));
    assert!(content.contains("**exec**"));
}

#[test]
fn finish_reason_label_appears_in_done_event_shape() {
    // Verify FinishReason labels are valid strings for the done event
    assert_eq!(agent::FinishReason::Complete.label(), "complete");
    assert_eq!(agent::FinishReason::Empty.label(), "empty");
}

#[test]
fn show_react_field_defaults_to_true_in_deserialized_session() {
    let json_str = r#"{
        "id": "test",
        "name": "Test",
        "messages": [],
        "created_at": 0,
        "updated_at": 0,
        "tool_calls_count": 0
    }"#;
    let session: Session = serde_json::from_str(json_str).unwrap();
    assert!(session.show_react);
}

#[test]
fn load_session_from_disk_migrates_show_react_to_true_for_older_sessions() {
    let session_id = format!("react-migrate-{}", now_epoch());
    let path = sessions_dir().join(format!("{session_id}.json"));
    let payload = json!({
        "id": session_id,
        "name": "Test",
        "messages": [],
        "created_at": 0,
        "updated_at": 0,
        "tool_calls_count": 0,
        "show_react": false,
        "version": 1
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&payload).expect("payload should serialize"),
    )
    .expect("session file should be written");

    let session = load_session_from_disk(
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .expect("session id should be valid"),
    )
    .expect("session should load");

    assert!(session.show_react);
    assert_eq!(session.version, SESSION_VERSION);

    let _ = std::fs::remove_file(&path);
    let workspace = session_workspace_path(&session.id)
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn load_session_from_disk_migrates_tool_and_reasoning_visibility_to_true_for_older_sessions() {
    let session_id = format!("view-migrate-{}", now_epoch());
    let path = sessions_dir().join(format!("{session_id}.json"));
    let payload = json!({
        "id": session_id,
        "name": "Test",
        "messages": [],
        "created_at": 0,
        "updated_at": 0,
        "tool_calls_count": 0,
        "show_react": true,
        "version": 2
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&payload).expect("payload should serialize"),
    )
    .expect("session file should be written");

    let session = load_session_from_disk(
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .expect("session id should be valid"),
    )
    .expect("session should load");

    assert!(session.show_tools);
    assert!(session.show_reasoning);
    assert_eq!(session.version, SESSION_VERSION);

    let _ = std::fs::remove_file(&path);
    let workspace = session_workspace_path(&session.id)
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn handle_command_persists_tool_and_reasoning_visibility_changes() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let session_id = format!("persist-view-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let mut session = test_session(&session_id, "Persist View", None);
    session.workspace = workspace.clone();
    session.version = SESSION_VERSION;

    let state = test_app_state();
    {
        let mut sessions = rt.block_on(state.sessions.lock());
        sessions.insert(session_id.clone(), session);
    }

    let (tx, _rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();

    let tool_result = rt
        .block_on(handle_command(
            "/tool off",
            &session_id,
            1,
            &state,
            &tx,
            &cancel,
        ))
        .expect("command should return a result");
    assert_eq!(tool_result.response_type, "system");
    assert!(tool_result.sessions_changed);
    assert!(tool_result.refresh_history);

    let reasoning_result = rt
        .block_on(handle_command(
            "/reasoning off",
            &session_id,
            1,
            &state,
            &tx,
            &cancel,
        ))
        .expect("command should return a result");
    assert_eq!(reasoning_result.response_type, "system");
    assert!(reasoning_result.sessions_changed);
    assert!(!reasoning_result.refresh_history);

    let persisted = load_session_from_disk(&session_id).expect("session should load from disk");
    assert!(!persisted.show_tools);
    assert!(!persisted.show_reasoning);

    let path = sessions_dir().join(format!("{session_id}.json"));
    let _ = std::fs::remove_file(path);
    let session_dir = workspace
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(session_dir);
}

#[test]
fn handle_command_persists_model_think_and_react_changes() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let session_id = format!("persist-command-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let mut session = test_session(&session_id, "Before Rename", None);
    session.workspace = workspace.clone();
    session.version = SESSION_VERSION;

    let state = test_app_state();
    {
        let mut sessions = rt.block_on(state.sessions.lock());
        sessions.insert(session_id.clone(), session);
    }

    let (tx, _rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();

    let model_result = rt
        .block_on(handle_command(
            "/model openai/gpt-4o-mini",
            &session_id,
            1,
            &state,
            &tx,
            &cancel,
        ))
        .expect("command should return a result");
    assert_eq!(model_result.response_type, "system");
    assert!(model_result.sessions_changed);

    let think_result = rt
        .block_on(handle_command(
            "/think high",
            &session_id,
            1,
            &state,
            &tx,
            &cancel,
        ))
        .expect("command should return a result");
    assert_eq!(think_result.response_type, "system");
    assert!(think_result.sessions_changed);

    let react_result = rt
        .block_on(handle_command(
            "/react off",
            &session_id,
            1,
            &state,
            &tx,
            &cancel,
        ))
        .expect("command should return a result");
    assert_eq!(react_result.response_type, "system");
    assert!(react_result.sessions_changed);

    let persisted = load_session_from_disk(&session_id).expect("session should load from disk");
    assert_eq!(
        persisted.model_override.as_deref(),
        Some("openai/gpt-4o-mini")
    );
    assert_eq!(persisted.think_level, "high");
    assert!(!persisted.show_react);
    assert_eq!(persisted.name, "Before Rename");
    assert!(persisted.updated_at > 0);

    let path = sessions_dir().join(format!("{session_id}.json"));
    let _ = std::fs::remove_file(path);
    let session_dir = workspace
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(session_dir);
}

#[test]
fn handle_command_persists_clear_changes() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let session_id = format!("persist-clear-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let mut session = test_session(&session_id, "Persist Clear", None);
    session.workspace = workspace.clone();
    session.version = SESSION_VERSION;
    session.messages.push(make_message("user", "keep me?"));
    session.messages.push(make_message("assistant", "no"));
    session.todos = crate::todos::TodoSnapshot {
        revision: 4,
        items: vec![crate::todos::TodoItem {
            id: "todo-1".into(),
            content: "stale item".into(),
            status: crate::todos::TodoStatus::InProgress,
        }],
        last_updated_by: crate::todos::TodoUpdatedBy::User,
        updated_at: 44,
    };

    let state = test_app_state();
    {
        let mut sessions = rt.block_on(state.sessions.lock());
        sessions.insert(session_id.clone(), session.clone());
    }
    rt.block_on(save_session_to_disk(&session))
        .expect("session should be saved before clear");

    let (tx, _rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();

    let clear_result = rt
        .block_on(handle_command(
            "/clear",
            &session_id,
            1,
            &state,
            &tx,
            &cancel,
        ))
        .expect("command should return a result");
    assert_eq!(clear_result.response_type, "system");
    assert!(clear_result.sessions_changed);
    assert!(clear_result.refresh_history);

    let persisted = load_session_from_disk(&session_id).expect("session should load from disk");
    assert_eq!(persisted.messages.len(), 1);
    assert_eq!(persisted.messages[0].role, "system");
    assert_eq!(persisted.tool_calls_count, 0);
    assert_eq!(persisted.todos.revision, 5);
    assert!(persisted.todos.items.is_empty());
    assert_eq!(
        persisted.todos.last_updated_by,
        crate::todos::TodoUpdatedBy::User
    );
    assert!(persisted.updated_at > 0);

    let stale_todo_write = rt
        .block_on(crate::todos::replace_session_todos(
            &state,
            &session_id,
            crate::todos::TodoReplaceRequest {
                base_revision: 4,
                items: vec![crate::todos::TodoItem {
                    id: "todo-stale".into(),
                    content: "should not return".into(),
                    status: crate::todos::TodoStatus::Pending,
                }],
            },
            crate::todos::TodoUpdateOrigin::Assistant,
        ))
        .expect("stale write should return conflict snapshot");
    assert!(stale_todo_write.conflict);
    assert_eq!(stale_todo_write.revision, 5);
    assert!(stale_todo_write.items.is_empty());

    let path = sessions_dir().join(format!("{session_id}.json"));
    let _ = std::fs::remove_file(path);
    let session_dir = workspace
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(session_dir);
}

#[test]
fn handle_command_persists_new_on_empty_context() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let session_id = format!("persist-new-empty-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let mut session = test_session(&session_id, "Persist New Empty", None);
    session.workspace = workspace.clone();
    session.version = SESSION_VERSION;

    let state = test_app_state();
    {
        let mut sessions = rt.block_on(state.sessions.lock());
        sessions.insert(session_id.clone(), session.clone());
    }
    rt.block_on(save_session_to_disk(&session))
        .expect("session should be saved before new");

    let (tx, _rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();

    let new_result = rt
        .block_on(handle_command("/new", &session_id, 1, &state, &tx, &cancel))
        .expect("command should return a result");
    assert_eq!(new_result.response_type, "system");
    assert!(new_result.sessions_changed);
    assert!(new_result.refresh_history);

    let persisted = load_session_from_disk(&session_id).expect("session should load from disk");
    assert_eq!(persisted.messages.len(), 1);
    assert_eq!(persisted.messages[0].role, "system");
    assert_eq!(persisted.tool_calls_count, 0);
    assert!(persisted.updated_at > 0);

    let path = sessions_dir().join(format!("{session_id}.json"));
    let _ = std::fs::remove_file(path);
    let session_dir = workspace
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(session_dir);
}

#[test]
fn handle_command_new_persists_existing_auto_summary_to_memory() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let session_id = format!("persist-new-auto-summary-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let mut session = test_session(&session_id, "Persist New Auto Summary", None);
    session.workspace = workspace.clone();
    session.version = SESSION_VERSION;
    session
        .messages
        .push(crate::hooks::build_auto_summary_message(
            "Recovered summary from auto compression.",
        ));

    let state = test_app_state();
    {
        let mut sessions = rt.block_on(state.sessions.lock());
        sessions.insert(session_id.clone(), session.clone());
    }
    rt.block_on(save_session_to_disk(&session))
        .expect("session should be saved before new");

    let (tx, _rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();

    let new_result = rt
        .block_on(handle_command("/new", &session_id, 1, &state, &tx, &cancel))
        .expect("command should return a result");
    assert_eq!(new_result.response_type, "system");
    assert!(new_result.sessions_changed);
    assert!(new_result.refresh_history);

    let persisted = load_session_from_disk(&session_id).expect("session should load from disk");
    assert_eq!(persisted.messages.len(), 1);
    assert_eq!(persisted.messages[0].role, "system");
    assert!(
        new_result
            .response
            .contains("Conversation compressed and saved to memory/")
    );

    let local_snapshot = prompts::current_local_snapshot();
    let today = local_snapshot.today();
    let memory_path = workspace.join("memory").join(format!("{today}.md"));
    let memory = std::fs::read_to_string(&memory_path).expect("memory file should exist");
    assert!(memory.contains("Recovered summary from auto compression."));
    assert!(!memory.contains("## Context Summary (auto-generated)"));

    let path = sessions_dir().join(format!("{session_id}.json"));
    let _ = std::fs::remove_file(path);
    let session_dir = workspace
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(session_dir);
}

#[test]
fn handle_command_switch_is_blocked_in_single_session_mode() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let source_id = format!("abandon-empty-switch-source-{}", now_epoch());
    let source_workspace = session_workspace_path(&source_id);
    std::fs::create_dir_all(&source_workspace).expect("source workspace should be created");

    let mut source = test_session(&source_id, "Empty Source", None);
    source.workspace = source_workspace.clone();
    source.version = SESSION_VERSION;

    let target_id = format!("abandon-empty-switch-target-{}", now_epoch());
    let target_workspace = session_workspace_path(&target_id);
    std::fs::create_dir_all(&target_workspace).expect("target workspace should be created");

    let mut target = test_session(&target_id, "Target Session", None);
    target.workspace = target_workspace.clone();
    target.version = SESSION_VERSION;

    let state = test_app_state();
    {
        let mut sessions = rt.block_on(state.sessions.lock());
        sessions.insert(source_id.clone(), source.clone());
        sessions.insert(target_id.clone(), target.clone());
    }
    rt.block_on(save_session_to_disk(&source))
        .expect("empty source session should be saved");
    rt.block_on(save_session_to_disk(&target))
        .expect("target session should be saved");

    let (tx, _rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();

    let result = rt
        .block_on(handle_command(
            &format!("/switch {target_id}"),
            &source_id,
            1,
            &state,
            &tx,
            &cancel,
        ))
        .expect("command should return a result");

    assert_eq!(
        result.response,
        format!("Switching to session: {target_id}")
    );
    assert_eq!(
        result.switch_to_session.as_deref(),
        Some(target_id.as_str())
    );
    assert!(sessions_dir().join(format!("{source_id}.json")).exists());
    assert!(
        source_workspace
            .parent()
            .expect("source session dir should exist")
            .exists()
    );

    let target_path = sessions_dir().join(format!("{target_id}.json"));
    let _ = std::fs::remove_file(target_path);
    let target_session_dir = target_workspace
        .parent()
        .map(PathBuf::from)
        .expect("target session dir should exist");
    let _ = std::fs::remove_dir_all(target_session_dir);
}

#[test]
fn recoverable_session_ids_skip_empty_and_corrupt_sessions() {
    let summaries = vec![
        crate::session_store::SessionSummary {
            id: "empty-session".to_string(),
            name: "Empty".to_string(),
            messages: 0,
            tool_calls: 0,
            created_at: 0,
            updated_at: 0,
            corrupt: false,
        },
        crate::session_store::SessionSummary {
            id: "corrupt-session".to_string(),
            name: "[Corrupt Session]".to_string(),
            messages: 99,
            tool_calls: 0,
            created_at: 0,
            updated_at: 0,
            corrupt: true,
        },
        crate::session_store::SessionSummary {
            id: "real-session".to_string(),
            name: "Real".to_string(),
            messages: 3,
            tool_calls: 0,
            created_at: 0,
            updated_at: 0,
            corrupt: false,
        },
    ];

    let recoverable = recoverable_session_ids_from_summaries(&summaries);

    assert_eq!(recoverable, vec!["real-session".to_string()]);
}

#[test]
fn resolve_session_target_for_command_accepts_persisted_empty_session_prefix() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("empty-prefix-session-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    let _guard = SavedSessionGuard {
        session_id: session_id.clone(),
        workspace,
    };

    let persisted_session = Session {
        id: session_id.clone(),
        name: "Empty Persisted".to_string(),
        messages: Vec::new(),
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
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: session_workspace_path(&session_id),
    };
    rt.block_on(save_session_to_disk(&persisted_session))
        .expect("session should persist");

    let prefix = &session_id[..session_id.len().min(12)];
    let resolved = rt
        .block_on(crate::runtime_loop::resolve_session_target_for_command(
            &state, prefix,
        ))
        .expect("empty persisted session prefix should resolve");

    assert_eq!(resolved, session_id);
}

#[test]
fn finalize_connection_removes_unbound_session_from_memory() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let session_id = format!("finalize-cleanup-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let mut session = test_session(&session_id, "Finalize Cleanup", None);
    session.workspace = workspace.clone();
    session.version = SESSION_VERSION;

    let state = test_app_state();
    {
        let mut sessions = rt.block_on(state.sessions.lock());
        sessions.insert(session_id.clone(), session);
    }
    {
        let mut active = rt.block_on(state.active_connections.lock());
        active.insert(session_id.clone(), 7);
    }

    let connection_cancel = CancellationToken::new();
    let (tx, _rx) = mpsc::channel::<String>(4);
    let (live_tx, _live_rx) = mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let disconnect_watcher = rt.spawn(async {});
    let live_dispatcher = rt.spawn(async {});
    let reader = rt.spawn(async {});
    let writer = rt.spawn(async {});

    rt.block_on(finalize_connection(
        &state,
        &session_id,
        7,
        &connection_cancel,
        ConnectionCleanup {
            tx,
            live_tx,
            tasks: socket_tasks::SocketTaskHandles {
                live_dispatcher,
                disconnect_watcher,
            },
            reader,
            writer,
        },
    ));

    assert!(
        rt.block_on(state.sessions.lock())
            .get(&session_id)
            .is_none()
    );
    assert!(
        rt.block_on(state.active_connections.lock())
            .get(&session_id)
            .is_none()
    );

    let path = sessions_dir().join(format!("{session_id}.json"));
    let _ = std::fs::remove_file(path);
    let session_dir = workspace
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(session_dir);
}

#[test]
fn finalize_connection_keeps_main_session_loaded_in_memory() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let session_id = MAIN_SESSION_ID.to_string();
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let mut session = test_session(&session_id, "Main", None);
    session.workspace = workspace.clone();
    session.version = SESSION_VERSION;

    let state = test_app_state();
    {
        let mut sessions = rt.block_on(state.sessions.lock());
        sessions.insert(session_id.clone(), session);
    }
    {
        let mut active = rt.block_on(state.active_connections.lock());
        active.insert(session_id.clone(), 7);
    }

    let connection_cancel = CancellationToken::new();
    let (tx, _rx) = mpsc::channel::<String>(4);
    let (live_tx, _live_rx) = mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let disconnect_watcher = rt.spawn(async {});
    let live_dispatcher = rt.spawn(async {});
    let reader = rt.spawn(async {});
    let writer = rt.spawn(async {});

    rt.block_on(finalize_connection(
        &state,
        &session_id,
        7,
        &connection_cancel,
        ConnectionCleanup {
            tx,
            live_tx,
            tasks: socket_tasks::SocketTaskHandles {
                live_dispatcher,
                disconnect_watcher,
            },
            reader,
            writer,
        },
    ));

    assert!(
        rt.block_on(state.sessions.lock())
            .get(&session_id)
            .is_some()
    );
    assert!(
        rt.block_on(state.active_connections.lock())
            .get(&session_id)
            .is_none()
    );

    let path = sessions_dir().join(format!("{session_id}.json"));
    let _ = std::fs::remove_file(path);
    let session_dir = workspace
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(session_dir);
}

#[test]
fn switch_socket_session_binds_new_session_before_replay_so_live_events_are_not_lost() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let previous_session_id = MAIN_SESSION_ID.to_string();
    let next_session_id = format!("switch-live-session-{}", now_epoch());
    let next_workspace = session_workspace_path(&next_session_id);
    std::fs::create_dir_all(&next_workspace).expect("workspace should be created");
    let _guard = SavedSessionGuard {
        session_id: next_session_id.clone(),
        workspace: next_workspace,
    };

    let mut previous_session = test_session(&previous_session_id, "Main", None);
    previous_session.workspace = session_workspace_path(&previous_session_id);
    let mut next_session = test_session(&next_session_id, "Switch Target", None);
    next_session.workspace = session_workspace_path(&next_session_id);
    rt.block_on(async {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(previous_session_id.clone(), previous_session);
        sessions.insert(next_session_id.clone(), next_session);
    });

    let (tx, mut rx) = mpsc::channel::<String>(16);
    let current_session_ref = Arc::new(Mutex::new(previous_session_id.clone()));
    let mut current_session_id = previous_session_id.clone();
    let connection_cancel = CancellationToken::new();

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &previous_session_id,
        1,
        &tx,
        true,
    ));
    rt.block_on(async {
        state.connection_cancels.lock().await.insert(
            previous_session_id.clone(),
            ConnectionCancelBinding {
                connection_id: 1,
                cancel: connection_cancel.clone(),
            },
        );
        state.active_runs.lock().await.insert(
            next_session_id.clone(),
            SessionRunBinding {
                connection_id: 99,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
    });
    rt.block_on(dispatch_live_event(
        state.as_ref(),
        &next_session_id,
        99,
        json!({"type":"start","round":1,"phase":"act","cycle":0,"react_visible":false}),
    ));

    rt.block_on(switch_socket_session(
        state.as_ref(),
        &tx,
        &current_session_ref,
        &mut current_session_id,
        &connection_cancel,
        1,
        next_session_id.clone(),
    ))
    .expect("session switch should succeed");

    rt.block_on(dispatch_live_event(
        state.as_ref(),
        &next_session_id,
        99,
        json!({"type":"delta","content":"tail after switch"}),
    ));

    let payloads = rt.block_on(async {
        let mut events = Vec::new();
        for _ in 0..6 {
            let payload = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
                .await
                .expect("payload should arrive before timeout")
                .expect("channel should stay open during switch replay");
            events.push(serde_json::from_str::<serde_json::Value>(&payload).expect("payload json"));
        }
        events
    });

    assert!(payloads.iter().any(|event| event["type"] == "session"));
    assert!(payloads.iter().any(|event| event["type"] == "history"));
    assert!(payloads.iter().any(|event| event["type"] == "start"));
    assert!(
        payloads
            .iter()
            .any(|event| { event["type"] == "delta" && event["content"] == "tail after switch" })
    );
}

#[test]
fn finalize_connection_does_not_remove_newer_connection_cancel_binding() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("cancel-binding-{}", now_epoch());
    let old_cancel = CancellationToken::new();
    let newer_cancel = CancellationToken::new();

    {
        let mut active = rt.block_on(state.active_connections.lock());
        active.insert(session_id.clone(), 2);
    }
    {
        let mut cancels = rt.block_on(state.connection_cancels.lock());
        cancels.insert(
            session_id.clone(),
            ConnectionCancelBinding {
                connection_id: 2,
                cancel: newer_cancel.clone(),
            },
        );
    }

    let (tx, _rx) = mpsc::channel::<String>(4);
    let (live_tx, _live_rx) = mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let disconnect_watcher = rt.spawn(async {});
    let live_dispatcher = rt.spawn(async {});
    let reader = rt.spawn(async {});
    let writer = rt.spawn(async {});

    rt.block_on(finalize_connection(
        &state,
        &session_id,
        1,
        &old_cancel,
        ConnectionCleanup {
            tx,
            live_tx,
            tasks: socket_tasks::SocketTaskHandles {
                live_dispatcher,
                disconnect_watcher,
            },
            reader,
            writer,
        },
    ));

    let active = rt.block_on(state.active_connections.lock());
    assert_eq!(active.get(&session_id).copied(), Some(2));

    let cancels = rt.block_on(state.connection_cancels.lock());
    let binding = cancels
        .get(&session_id)
        .expect("newer connection cancel binding should remain");
    assert_eq!(binding.connection_id, 2);
    assert!(!binding.cancel.is_cancelled());
}

#[test]
fn help_command_lists_usage_without_extra_indent() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let (tx, _rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();

    let result = rt
        .block_on(handle_command(
            "/help",
            MAIN_SESSION_ID,
            1,
            &state,
            &tx,
            &cancel,
        ))
        .expect("command should return a result");

    assert!(
        result
            .response
            .contains("  /status          Show session status")
    );
    assert!(
        result
            .response
            .contains("/system-prompt   Show current system prompt and estimated tokens")
    );
    assert!(
        result
            .response
            .contains("/mcp [refresh]   Show MCP load status or refresh cache")
    );
    assert!(
        result
            .response
            .contains("/usage           Show session token usage")
    );
}

#[test]
fn handle_command_reports_mcp_load_failures() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let session_id = format!("mcp-command-{}", now_epoch());
    let workspace = session_workspace_path(&session_id);
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let mut session = test_session(&session_id, "MCP Status", None);
    session.workspace = workspace.clone();
    session.version = SESSION_VERSION;

    let mut config = test_config();
    config.mcp_servers.insert(
        "broken".to_string(),
        JsonMcpServerConfig {
            command: "definitely-not-a-real-command".to_string(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
            enabled: true,
            timeout_secs: Some(1),
        },
    );

    let state = test_app_state_with_config(config);
    {
        let mut sessions = rt.block_on(state.sessions.lock());
        sessions.insert(session_id.clone(), session);
    }

    let (tx, _rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();

    let result = rt
        .block_on(handle_command("/mcp", &session_id, 1, &state, &tx, &cancel))
        .expect("command should return a result");

    assert_eq!(result.response_type, "system");
    assert!(result.response.contains("MCP servers:"));
    assert!(result.response.contains("- broken: failed to load"));
    assert!(
        result
            .response
            .contains("failed to spawn 'definitely-not-a-real-command'")
    );

    let path = sessions_dir().join(format!("{session_id}.json"));
    let _ = std::fs::remove_file(path);
    let session_dir = workspace
        .parent()
        .map(PathBuf::from)
        .expect("session dir should exist");
    let _ = std::fs::remove_dir_all(session_dir);
}

#[test]
fn finish_session_replay_disconnects_slow_client_when_writer_queue_stays_full() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-replay-flush-{}", now_epoch());
    let writer_capacity = 2;
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(writer_capacity);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        state.as_ref(),
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(dispatch_live_event(
        state.as_ref(),
        &session_id,
        1,
        json!({"type":"delta","content":"replayed first"}),
    ));

    rt.block_on(async {
        for _ in 0..writer_capacity {
            bound_tx
                .send(json!({"type":"sentinel"}).to_string())
                .await
                .expect("sentinel should fill the writer queue");
        }
    });

    rt.block_on(finish_session_replay(state.as_ref(), &session_id, 1));

    for _ in 0..writer_capacity {
        assert_eq!(
            bound_rx
                .try_recv()
                .expect("sentinel should still be queued"),
            json!({"type":"sentinel"}).to_string()
        );
    }
    assert!(matches!(
        bound_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    rt.block_on(async {
        let clients = state.session_clients.lock().await;
        assert!(
            !clients.contains_key(&session_id),
            "persistently full writer queue should disconnect the slow client"
        );
    });
}

#[test]
fn take_live_client_events_for_send_drops_slow_client_when_backlog_overflows() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-backlog-overflow-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(8);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        false,
    ));

    rt.block_on(async {
        let mut clients = state.session_clients.lock().await;
        let binding = clients
            .get_mut(&session_id)
            .expect("session client binding should exist");
        binding.pending_events = (0..MAX_PENDING_LIVE_CLIENT_EVENTS)
            .map(|idx| json!({"type":"delta","content":format!("queued-{idx}")}))
            .collect();
    });

    let next = rt.block_on(take_live_client_events_for_send(
        state.as_ref(),
        &session_id,
        1,
        json!({"type":"delta","content":"overflow"}),
    ));

    let Some(QueueLiveClientEventsResult::Disconnect(SlowClientDisconnect { connection_id })) =
        next
    else {
        panic!("overflow should return a disconnect result");
    };
    assert_eq!(connection_id, 1);
}

#[test]
fn disconnect_session_connection_if_matches_cancels_matching_socket() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("slow-client-disconnect-{}", now_epoch());
    let cancel = CancellationToken::new();

    rt.block_on(async {
        state
            .active_connections
            .lock()
            .await
            .insert(session_id.clone(), 1);
        let (bound_tx, _bound_rx) = mpsc::channel::<String>(8);
        state.session_clients.lock().await.insert(
            session_id.clone(),
            SessionClientBinding {
                connection_id: 1,
                tx: bound_tx,
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );
        state.connection_cancels.lock().await.insert(
            session_id.clone(),
            ConnectionCancelBinding {
                connection_id: 1,
                cancel: cancel.clone(),
            },
        );
    });

    rt.block_on(disconnect_session_connection_if_matches(
        state.as_ref(),
        &session_id,
        1,
    ));

    assert!(cancel.is_cancelled());
    rt.block_on(async {
        assert!(
            !state
                .active_connections
                .lock()
                .await
                .contains_key(&session_id)
        );
        assert!(!state.session_clients.lock().await.contains_key(&session_id));
    });
}

#[test]
fn disconnect_session_connection_if_matches_keeps_newer_socket_alive() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("slow-client-disconnect-newer-{}", now_epoch());
    let current_cancel = CancellationToken::new();
    let newer_cancel = CancellationToken::new();

    rt.block_on(async {
        state
            .active_connections
            .lock()
            .await
            .insert(session_id.clone(), 2);
        let (current_tx, _current_rx) = mpsc::channel::<String>(8);
        state.session_clients.lock().await.insert(
            session_id.clone(),
            SessionClientBinding {
                connection_id: 2,
                tx: current_tx,
                replay_ready: true,
                pending_events: VecDeque::new(),
                live_send_in_progress: false,
            },
        );
        state.connection_cancels.lock().await.insert(
            session_id.clone(),
            ConnectionCancelBinding {
                connection_id: 2,
                cancel: newer_cancel.clone(),
            },
        );
    });

    rt.block_on(disconnect_session_connection_if_matches(
        state.as_ref(),
        &session_id,
        1,
    ));

    assert!(!current_cancel.is_cancelled());
    assert!(!newer_cancel.is_cancelled());
    rt.block_on(async {
        assert_eq!(
            state
                .active_connections
                .lock()
                .await
                .get(&session_id)
                .copied(),
            Some(2)
        );
        assert_eq!(
            state
                .session_clients
                .lock()
                .await
                .get(&session_id)
                .map(|binding| binding.connection_id),
            Some(2)
        );
    });
}

#[test]
fn finish_session_replay_drains_backlog_larger_than_writer_queue() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-replay-drain-{}", now_epoch());
    let writer_capacity = 2;
    let backlog_len = 5;
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(writer_capacity);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        false,
    ));

    for index in 0..backlog_len {
        rt.block_on(dispatch_live_event(
            state.as_ref(),
            &session_id,
            1,
            json!({"type":"delta","content":format!("event-{index}")}),
        ));
    }

    let flush_state = Arc::clone(&state);
    let flush_session_id = session_id.clone();
    let received = rt.block_on(async move {
        let flush = tokio::spawn(async move {
            finish_session_replay(flush_state.as_ref(), &flush_session_id, 1).await;
        });

        let mut received = Vec::new();
        for _ in 0..backlog_len {
            let raw = tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
                .await
                .expect("replayed event should arrive before timeout")
                .expect("bound client should receive replayed event");
            received.push(
                serde_json::from_str::<serde_json::Value>(&raw)
                    .expect("payload should be valid json"),
            );
        }

        flush.await.expect("replay flush task should complete");
        received
    });

    for (index, event) in received.iter().enumerate() {
        assert_eq!(event["type"], "delta");
        assert_eq!(event["content"], format!("event-{index}"));
    }

    rt.block_on(async {
        let clients = state.session_clients.lock().await;
        let binding = clients
            .get(&session_id)
            .expect("session client binding should exist");
        assert!(binding.pending_events.is_empty());
        assert!(!binding.live_send_in_progress);
    });
}

#[test]
fn replay_live_round_rehydrates_inflight_round_state() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-replay-{}", now_epoch());
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 3,
            "phase": "act",
            "cycle": 2,
            "react_visible": true,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "auto_trace",
            "round": 3,
            "cycle": 2,
            "phase": "act",
            "model": "openai/gpt-4o-reasoner",
            "provider": "openai",
            "selected_think": "high",
            "baseline_level": "medium",
            "baseline_reason": "mid_loop_investigate",
            "escalators": ["stagnation_streak"],
            "dampeners": [],
            "clamps": [],
            "signals": {
                "intent": "investigate",
                "user_msg_chars": 96,
                "observation_strength": "medium",
                "tool_results_count": 2,
                "tool_error_count": 0,
                "summary_count": 1,
                "summary_bytes": 4096,
                "stagnation_streak": 1,
                "error_streak": 0,
                "task_pressure": 2,
                "ready_to_finish": false,
                "action_oriented": true,
                "has_blocking_uncertainty": false,
                "progress_made": false,
                "retry_pattern": "none",
                "error_kind": "none",
                "evidence_delta_quality": "more_evidence"
            }
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type": "thinking_start"}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type": "thinking_delta", "content": "step-1"}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type": "thinking_done"}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "tool_call",
            "id": "tool-1",
            "name": "read_file",
            "arguments": "{\"path\":\"README.md\"}",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "tool_output",
            "id": "tool-1",
            "name": "read_file",
            "stream": "stderr",
            "chunk": "partial output",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "tool_result",
            "id": "tool-1",
            "name": "read_file",
            "result": "file contents",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type": "delta", "content": "final answer"}),
    ));

    assert!(bound_rx.try_recv().is_err());

    rt.block_on(finish_session_replay(&state, &session_id, 1));

    for _ in 0..9 {
        let _ = rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(2), bound_rx.recv()).await })
            .expect("bound replay event should arrive before timeout")
            .expect("bound client should receive live event");
    }

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed = (0..9)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replayed event should arrive before timeout")
                .expect("replay should produce serialized event");
            serde_json::from_str::<serde_json::Value>(&raw)
                .expect("replayed event should be valid json")
        })
        .collect::<Vec<_>>();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[0]["round"], 3);
    assert_eq!(replayed[0]["phase"], "act");
    assert_eq!(replayed[0]["cycle"], 2);
    assert_eq!(replayed[0]["react_visible"], true);
    assert_eq!(replayed[1]["type"], "auto_trace");
    assert_eq!(replayed[1]["selected_think"], "high");
    assert_eq!(replayed[1]["baseline_reason"], "mid_loop_investigate");
    assert_eq!(replayed[1]["signals"]["intent"], "investigate");
    assert!(
        replayed[1]["signals"]
            .get("finish_deferral_count")
            .is_none()
    );
    assert_eq!(replayed[2]["type"], "thinking_start");
    assert_eq!(replayed[3]["type"], "thinking_delta");
    assert_eq!(replayed[3]["content"], "step-1");
    assert_eq!(replayed[4]["type"], "thinking_done");
    assert_eq!(replayed[5]["type"], "tool_call");
    assert_eq!(replayed[5]["id"], "tool-1");
    assert_eq!(replayed[6]["type"], "tool_output");
    assert_eq!(replayed[6]["id"], "tool-1");
    assert!(replayed[6].get("stream").is_none());
    assert_eq!(replayed[6]["chunk"], "\n[stderr]\npartial output");
    assert_eq!(replayed[7]["type"], "tool_result");
    assert_eq!(replayed[7]["result"], "file contents");
    assert_eq!(replayed[8]["type"], "delta");
    assert_eq!(replayed[8]["content"], "final answer");

    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type": "done"}),
    ));
    assert!(
        rt.block_on(state.live_rounds.lock())
            .get(&session_id)
            .is_none()
    );
}

#[test]
fn record_tool_output_event_disconnects_slow_client_socket_on_overflow() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("tool-output-overflow-disconnect-{}", now_epoch());
    let cancel = CancellationToken::new();

    rt.block_on(async {
        let (bound_tx, _bound_rx) = mpsc::channel::<String>(8);
        bind_session_connection(state.as_ref(), &session_id, 1, &bound_tx, false).await;
        let mut clients = state.session_clients.lock().await;
        let binding = clients
            .get_mut(&session_id)
            .expect("session client binding should exist");
        binding.pending_events = (0..MAX_PENDING_LIVE_CLIENT_EVENTS)
            .map(|idx| json!({"type":"delta","content":format!("queued-{idx}")}))
            .collect();
        state.connection_cancels.lock().await.insert(
            session_id.clone(),
            ConnectionCancelBinding {
                connection_id: 1,
                cancel: cancel.clone(),
            },
        );
    });

    rt.block_on(record_tool_output_event_for_replay_and_client(
        state.as_ref(),
        &session_id,
        json!({
            "type": "tool_output",
            "id": "tool-1",
            "name": "exec",
            "stream": "stdout",
            "chunk": "overflow",
        }),
    ));

    assert!(cancel.is_cancelled());
    rt.block_on(async {
        assert!(!state.session_clients.lock().await.contains_key(&session_id));
        assert!(
            !state
                .active_connections
                .lock()
                .await
                .contains_key(&session_id)
        );
    });
}

#[test]
fn record_tool_output_event_only_synthesizes_missing_tool_call_once() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-tool-output-synth-{}", now_epoch());
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        true,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    let _ = bound_rx.try_recv();

    rt.block_on(record_tool_output_event_for_replay_and_client(
        &state,
        &session_id,
        json!({
            "type": "tool_output",
            "id": "tool-1",
            "name": "exec",
            "stream": "stdout",
            "chunk": "partial",
        }),
    ));

    let synthetic_tool_call = serde_json::from_str::<serde_json::Value>(&rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("synthetic tool_call should arrive before timeout")
            .expect("synthetic tool_call should be delivered")
    }))
    .expect("synthetic tool_call should parse");
    let tool_output = serde_json::from_str::<serde_json::Value>(&rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("tool_output should arrive before timeout")
            .expect("tool_output should be delivered")
    }))
    .expect("tool_output should parse");

    assert_eq!(synthetic_tool_call["type"], "tool_call");
    assert_eq!(synthetic_tool_call["id"], "tool-1");
    assert_eq!(synthetic_tool_call["arguments"], "");
    assert_eq!(tool_output["type"], "tool_output");

    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "tool_call",
            "id": "tool-1",
            "name": "exec",
            "arguments": "{\"command\":\"echo hi\"}",
        }),
    ));
    let real_tool_call = serde_json::from_str::<serde_json::Value>(&rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("real tool_call should arrive before timeout")
            .expect("real tool_call should be delivered")
    }))
    .expect("real tool_call should parse");
    assert_eq!(real_tool_call["type"], "tool_call");

    rt.block_on(record_tool_output_event_for_replay_and_client(
        &state,
        &session_id,
        json!({
            "type": "tool_output",
            "id": "tool-1",
            "name": "exec",
            "stream": "stdout",
            "chunk": " more",
        }),
    ));

    let next_event = serde_json::from_str::<serde_json::Value>(&rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("follow-up tool_output should arrive before timeout")
            .expect("follow-up tool_output should be delivered")
    }))
    .expect("follow-up tool_output should parse");
    assert_eq!(next_event["type"], "tool_output");
    assert!(matches!(
        bound_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    let live_round = rt.block_on(async {
        state
            .live_rounds
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .expect("live round should exist")
    });
    let tool = live_round
        .tools
        .iter()
        .find(|tool| tool.id == "tool-1")
        .expect("tool state should exist");
    assert_eq!(tool.arguments, "{\"command\":\"echo hi\"}");
}

#[test]
fn dispatch_live_event_ignores_subagent_start_for_parent_round() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-parent-start-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 3,
            "phase": "analyze",
            "cycle": 2,
            "model": "openai/gpt-4o-reasoner",
            "think_level": "medium",
            "react_visible": true,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "delta",
            "content": "parent output"
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "auto_trace",
            "round": 3,
            "cycle": 2,
            "phase": "analyze",
            "model": "openai/gpt-4o-reasoner",
            "provider": "openai",
            "selected_think": "high",
            "baseline_level": "medium",
            "baseline_reason": "mid_loop_investigate",
            "escalators": ["stagnation_streak"],
            "dampeners": [],
            "clamps": [],
            "signals": {
                "intent": "investigate",
                "user_msg_chars": 96,
                "observation_strength": "medium",
                "tool_results_count": 2,
                "tool_error_count": 0,
                "summary_count": 1,
                "summary_bytes": 4096,
                "stagnation_streak": 1,
                "error_streak": 0,
                "task_pressure": 2,
                "ready_to_finish": false,
                "action_oriented": true,
                "has_blocking_uncertainty": false,
                "progress_made": false,
                "retry_pattern": "none",
                "error_kind": "none",
                "evidence_delta_quality": "more_evidence"
            }
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 1,
            "phase": "analyze",
            "cycle": 0,
            "model": "openai/gpt-4o-mini",
            "think_level": "low",
            "react_visible": true,
            "subagent": "coder",
            "task_id": "task-1",
        }),
    ));

    let live_round = rt.block_on(async {
        state
            .live_rounds
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .expect("live round should exist")
    });

    assert_eq!(live_round.round, 3);
    assert_eq!(live_round.cycle, Some(2));
    assert_eq!(
        live_round.effective_model.as_deref(),
        Some("openai/gpt-4o-reasoner")
    );
    assert_eq!(live_round.effective_think.as_deref(), Some("high"));
    assert_eq!(live_round.assistant_text, "parent output");
    assert_eq!(
        live_round
            .latest_auto_trace
            .as_ref()
            .map(|trace| trace.selected_think.as_str()),
        Some("high")
    );
}

#[test]
fn dispatch_live_event_ignores_subagent_auto_trace_for_parent_round() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-parent-trace-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 3,
            "phase": "analyze",
            "cycle": 2,
            "model": "openai/gpt-4o-reasoner",
            "think_level": "medium",
            "react_visible": true,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "auto_trace",
            "round": 3,
            "cycle": 2,
            "phase": "analyze",
            "model": "openai/gpt-4o-reasoner",
            "provider": "openai",
            "selected_think": "high",
            "baseline_level": "medium",
            "baseline_reason": "mid_loop_investigate",
            "escalators": ["stagnation_streak"],
            "dampeners": [],
            "clamps": [],
            "signals": {
                "intent": "investigate",
                "user_msg_chars": 96,
                "observation_strength": "medium",
                "tool_results_count": 2,
                "tool_error_count": 0,
                "summary_count": 1,
                "summary_bytes": 4096,
                "stagnation_streak": 1,
                "error_streak": 0,
                "task_pressure": 2,
                "ready_to_finish": false,
                "action_oriented": true,
                "has_blocking_uncertainty": false,
                "progress_made": false,
                "retry_pattern": "none",
                "error_kind": "none",
                "evidence_delta_quality": "more_evidence"
            }
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "auto_trace",
            "round": 1,
            "cycle": 0,
            "phase": "analyze",
            "model": "openai/gpt-4o-mini",
            "provider": "openai",
            "selected_think": "low",
            "baseline_level": "low",
            "baseline_reason": "initial_inform",
            "escalators": [],
            "dampeners": [],
            "clamps": [],
            "subagent": "coder",
            "task_id": "task-1",
            "signals": {
                "intent": "inform",
                "user_msg_chars": 12,
                "observation_strength": "none",
                "tool_results_count": 0,
                "tool_error_count": 0,
                "summary_count": 0,
                "summary_bytes": 0,
                "stagnation_streak": 0,
                "error_streak": 0,
                "task_pressure": 0,
                "ready_to_finish": false,
                "action_oriented": false,
                "has_blocking_uncertainty": false,
                "progress_made": false,
                "retry_pattern": "none",
                "error_kind": "none",
                "evidence_delta_quality": "none"
            }
        }),
    ));

    let live_round = rt.block_on(async {
        state
            .live_rounds
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .expect("live round should exist")
    });

    assert_eq!(live_round.effective_think.as_deref(), Some("high"));
    assert_eq!(
        live_round
            .latest_auto_trace
            .as_ref()
            .map(|trace| trace.selected_think.as_str()),
        Some("high")
    );
    assert_eq!(
        live_round
            .latest_auto_trace
            .as_ref()
            .map(|trace| trace.model.as_str()),
        Some("openai/gpt-4o-reasoner")
    );
}

#[test]
fn dispatch_live_event_ignores_subagent_react_phase_for_parent_round() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-parent-phase-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 3,
            "phase": "analyze",
            "cycle": 2,
            "model": "openai/gpt-4o-reasoner",
            "think_level": "high",
            "react_visible": true,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "react_phase",
            "phase": "act",
            "cycle": 3,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "react_phase",
            "phase": "observe",
            "cycle": 7,
            "subagent": "coder",
            "task_id": "task-1",
        }),
    ));

    let live_round = rt.block_on(async {
        state
            .live_rounds
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .expect("live round should exist")
    });

    assert_eq!(live_round.phase.as_deref(), Some("act"));
    assert_eq!(live_round.cycle, Some(3));
}

#[test]
fn replay_live_round_rehydrates_active_task_with_task_id() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-task-replay-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 1,
            "phase": "act",
            "cycle": 1,
            "react_visible": true,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "task_started",
            "task_id": "task-123",
            "agent": "coder",
            "prompt": "Implement feature",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "task_progress",
            "task_id": "task-123",
            "agent": "coder",
            "cycle": 2,
            "phase": "analyze",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "task_tool",
            "task_id": "task-123",
            "agent": "coder",
            "tool": "read_file",
            "id": "tool-a",
        }),
    ));

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed = (0..4)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replay event should arrive before timeout")
                .expect("replay should produce serialized event");
            serde_json::from_str::<serde_json::Value>(&raw)
                .expect("replayed event should be valid json")
        })
        .collect::<Vec<_>>();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[1]["type"], "task_started");
    assert_eq!(replayed[1]["task_id"], "task-123");
    assert_eq!(replayed[1]["agent"], "coder");
    assert_eq!(replayed[2]["type"], "task_progress");
    assert_eq!(replayed[2]["task_id"], "task-123");
    assert_eq!(replayed[3]["type"], "task_tool");
    assert_eq!(replayed[3]["task_id"], "task-123");
    assert_eq!(replayed[3]["id"], "tool-a");
}

#[test]
fn replay_live_round_replaces_synthetic_task_start_with_real_prompt() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-task-start-replace-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 1,
            "phase": "act",
            "cycle": 1,
            "react_visible": true,
        }),
    ));
    rt.block_on(record_tool_output_event_for_replay_and_client(
        &state,
        &session_id,
        json!({
            "type": "tool_output",
            "task_id": "task-123",
            "subagent": "coder",
            "id": "tool-a",
            "name": "read_file",
            "stream": "stdout",
            "chunk": "partial subagent output",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "task_started",
            "task_id": "task-123",
            "agent": "coder",
            "prompt": "Implement feature",
        }),
    ));

    let live_round = rt.block_on(async {
        state
            .live_rounds
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .expect("live round should exist")
    });
    assert_eq!(live_round.delegated_events.len(), 2);
    assert_eq!(live_round.delegated_events[0]["type"], "task_started");
    assert_eq!(
        live_round.delegated_events[0]["prompt"],
        "Implement feature"
    );

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed = (0..3)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replay event should arrive before timeout")
                .expect("replay should produce serialized event");
            serde_json::from_str::<serde_json::Value>(&raw)
                .expect("replayed event should be valid json")
        })
        .collect::<Vec<_>>();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[1]["type"], "task_started");
    assert_eq!(replayed[1]["task_id"], "task-123");
    assert_eq!(replayed[1]["agent"], "coder");
    assert_eq!(replayed[1]["prompt"], "Implement feature");
    assert_eq!(replayed[2]["type"], "tool_output");
    assert_eq!(replayed[2]["task_id"], "task-123");
    assert_eq!(replayed[2]["subagent"], "coder");
    assert_eq!(replayed[2]["chunk"], "partial subagent output");
    assert!(matches!(
        replay_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[test]
fn replay_live_round_scopes_subagent_tool_results_to_task() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-task-tool-result-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 1,
            "phase": "act",
            "cycle": 1,
            "react_visible": true,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "task_started",
            "task_id": "task-123",
            "agent": "coder",
            "prompt": "Implement feature",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "thinking_start",
            "task_id": "task-123",
            "subagent": "coder",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "thinking_delta",
            "task_id": "task-123",
            "subagent": "coder",
            "content": "internal reasoning",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "delta",
            "task_id": "task-123",
            "subagent": "coder",
            "content": "subagent content",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "task_tool",
            "task_id": "task-123",
            "agent": "coder",
            "tool": "read_file",
            "id": "tool-a",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "tool_output",
            "task_id": "task-123",
            "subagent": "coder",
            "id": "tool-a",
            "name": "read_file",
            "stream": "stdout",
            "chunk": "partial subagent output",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "tool_result",
            "task_id": "task-123",
            "subagent": "coder",
            "id": "tool-a",
            "name": "read_file",
            "duration_ms": 42,
            "is_error": false,
        }),
    ));

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed = (0..7)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replay event should arrive before timeout")
                .expect("replay should produce serialized event");
            serde_json::from_str::<serde_json::Value>(&raw)
                .expect("replayed event should be valid json")
        })
        .collect::<Vec<_>>();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[1]["type"], "task_started");
    assert_eq!(replayed[2]["type"], "thinking_start");
    assert_eq!(replayed[2]["task_id"], "task-123");
    assert_eq!(replayed[2]["subagent"], "coder");
    assert_eq!(replayed[3]["type"], "thinking_delta");
    assert_eq!(replayed[3]["task_id"], "task-123");
    assert_eq!(replayed[3]["subagent"], "coder");
    assert_eq!(replayed[3]["content"], "internal reasoning");
    assert_eq!(replayed[4]["type"], "task_tool");
    assert_eq!(replayed[4]["task_id"], "task-123");
    assert_eq!(replayed[5]["type"], "tool_output");
    assert_eq!(replayed[5]["task_id"], "task-123");
    assert_eq!(replayed[5]["subagent"], "coder");
    assert_eq!(replayed[5]["id"], "tool-a");
    assert_eq!(replayed[5]["chunk"], "partial subagent output");
    assert_eq!(replayed[6]["type"], "tool_result");
    assert_eq!(replayed[6]["task_id"], "task-123");
    assert_eq!(replayed[6]["subagent"], "coder");
    assert_eq!(replayed[6]["id"], "tool-a");
    assert_eq!(replayed[6]["duration_ms"], 42);
    assert!(matches!(
        replay_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[test]
fn replay_ignores_orphaned_tool_result_after_task_completed() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-orphan-tool-result-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"task_started","task_id":"t-1","agent":"coder","prompt":"Do stuff"}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"task_completed","task_id":"t-1","agent":"coder","cycles":1,"tool_calls":0,"duration_ms":100}),
    ));
    // Late tool_result arrives after terminal event —should be silently dropped.
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"tool_result","task_id":"t-1","subagent":"coder","id":"orphan","name":"read_file","result":"late","duration_ms":10,"is_error":false}),
    ));

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed: Vec<serde_json::Value> = (0..3)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replayed event should arrive before timeout")
                .expect("replay should produce serialized event");
            serde_json::from_str(&raw).expect("valid json")
        })
        .collect();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[1]["type"], "task_started");
    assert_eq!(replayed[2]["type"], "task_completed");
    // No orphaned tool_result should be present.
    assert!(matches!(
        replay_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[test]
fn malformed_orchestrate_terminal_does_not_remove_unrelated_task() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-malformed-orch-terminal-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"task_started",
            "task_id":"coder",
            "agent":"standalone",
            "prompt":"Do stuff",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_started",
            "orchestrate_id":"orch-1",
            "task_count":1,
            "layer_count":1,
            "tasks":[
                {"id":"a","agent":"explore","depends_on":[]}
            ],
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_task_completed",
            "orchestrate_id":"orch-1",
            "agent":"coder",
            "cycles":1,
            "tool_calls":0,
            "duration_ms":10,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"task_completed",
            "task_id":"coder",
            "agent":"standalone",
            "cycles":1,
            "tool_calls":0,
            "duration_ms":20,
        }),
    ));

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed: Vec<serde_json::Value> = (0..4)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replay event should arrive before timeout")
                .expect("replay should produce event");
            serde_json::from_str(&raw).expect("valid json")
        })
        .collect();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[1]["type"], "task_started");
    assert_eq!(replayed[1]["task_id"], "coder");
    assert_eq!(replayed[2]["type"], "orchestrate_started");
    assert_eq!(replayed[3]["type"], "task_completed");
    assert_eq!(replayed[3]["task_id"], "coder");
    assert!(matches!(
        replay_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[test]
fn delegated_events_cap_prevents_unbounded_growth() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-delegated-cap-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    // Dispatch more task_started events than the cap allows.
    for i in 0..(DELEGATED_EVENTS_CAP + 500) {
        rt.block_on(dispatch_live_event(
            &state,
            &session_id,
            1,
            json!({
                "type":"task_started",
                "task_id": format!("t-{i}"),
                "agent":"coder",
                "prompt":"overflow test",
            }),
        ));
    }

    {
        let live_rounds = rt.block_on(state.live_rounds.lock());
        let round = live_rounds.get(&session_id).expect("round should exist");
        assert_eq!(
            round.delegated_events.len(),
            DELEGATED_EVENTS_CAP,
            "non-terminal events should be capped at DELEGATED_EVENTS_CAP"
        );
    }

    // Terminal events for tasks whose started event WAS stored (t-0..t-2)
    // should bypass the cap so the frontend can close their panels.
    let stored_terminal_count = 3;
    for i in 0..stored_terminal_count {
        rt.block_on(dispatch_live_event(
            &state,
            &session_id,
            1,
            json!({
                "type":"task_completed",
                "task_id": format!("t-{i}"),
                "agent":"coder",
                "cycles":1,"tool_calls":0,"duration_ms":10,
            }),
        ));
    }

    // Terminal events for tasks whose started event was NOT stored (past cap)
    // should be dropped —no panel exists on the frontend to close.
    for i in (DELEGATED_EVENTS_CAP)..(DELEGATED_EVENTS_CAP + 3) {
        rt.block_on(dispatch_live_event(
            &state,
            &session_id,
            1,
            json!({
                "type":"task_completed",
                "task_id": format!("t-{i}"),
                "agent":"coder",
                "cycles":1,"tool_calls":0,"duration_ms":5,
            }),
        ));
    }

    let live_rounds = rt.block_on(state.live_rounds.lock());
    let round = live_rounds.get(&session_id).expect("round should exist");
    assert_eq!(
        round.delegated_events.len(),
        DELEGATED_EVENTS_CAP + stored_terminal_count,
        "only terminal events with stored starts should bypass cap"
    );
    // Verify the bypass events are the stored-start terminals.
    for i in 0..stored_terminal_count {
        let event = &round.delegated_events[DELEGATED_EVENTS_CAP + i];
        assert_eq!(event["type"], "task_completed");
        assert_eq!(event["task_id"], format!("t-{i}"));
    }
}

#[test]
fn replay_live_round_preserves_subagent_tool_event_order() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-task-tool-order-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 1,
            "phase": "act",
            "cycle": 1,
            "react_visible": true,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "task_started",
            "task_id": "task-ordered",
            "agent": "coder",
            "prompt": "Implement feature",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "task_tool",
            "task_id": "task-ordered",
            "agent": "coder",
            "tool": "read_file",
            "id": "tool-a",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "tool_result",
            "task_id": "task-ordered",
            "subagent": "coder",
            "id": "tool-a",
            "name": "read_file",
            "duration_ms": 11,
            "is_error": false,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "task_tool",
            "task_id": "task-ordered",
            "agent": "coder",
            "tool": "list_dir",
            "id": "tool-b",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "tool_result",
            "task_id": "task-ordered",
            "subagent": "coder",
            "id": "tool-b",
            "name": "list_dir",
            "duration_ms": 22,
            "is_error": false,
        }),
    ));

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed = (0..6)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replay event should arrive before timeout")
                .expect("replay should produce serialized event");
            serde_json::from_str::<serde_json::Value>(&raw)
                .expect("replayed event should be valid json")
        })
        .collect::<Vec<_>>();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[1]["type"], "task_started");
    assert_eq!(replayed[2]["type"], "task_tool");
    assert_eq!(replayed[2]["id"], "tool-a");
    assert_eq!(replayed[3]["type"], "tool_result");
    assert_eq!(replayed[3]["id"], "tool-a");
    assert_eq!(replayed[4]["type"], "task_tool");
    assert_eq!(replayed[4]["id"], "tool-b");
    assert_eq!(replayed[5]["type"], "tool_result");
    assert_eq!(replayed[5]["id"], "tool-b");
    assert!(matches!(
        replay_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[test]
fn replay_live_round_rehydrates_active_orchestration_state() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-orch-replay-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 1,
            "phase": "act",
            "cycle": 1,
            "react_visible": true,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "orchestrate_started",
            "orchestrate_id": "orch-1",
            "task_count": 3,
            "layer_count": 2,
            "tasks": [
                {"id": "code", "agent": "explore", "depends_on": [], "prompt_preview": "Analyze codebase structure"},
                {"id": "docs", "agent": "researcher", "depends_on": [], "prompt_preview": "Read docs and changelog"},
                {"id": "plan", "agent": "coder", "depends_on": ["code", "docs"], "prompt_preview": "Draft final plan"}
            ],
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "orchestrate_layer",
            "orchestrate_id": "orch-1",
            "layer": 1,
            "total_layers": 2,
            "tasks": ["code", "docs"],
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "orchestrate_task_started",
            "orchestrate_id": "orch-1",
            "id": "code",
            "agent": "explore",
            "prompt": "Analyze code",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "orchestrate_task_started",
            "orchestrate_id": "orch-1",
            "id": "docs",
            "agent": "researcher",
            "prompt": "Read docs",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "task_progress",
            "task_id": "orch-1:docs",
            "agent": "researcher",
            "cycle": 2,
            "phase": "analyze",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "task_tool",
            "task_id": "orch-1:docs",
            "agent": "researcher",
            "tool": "grep_search",
            "id": "tool-docs",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "orchestrate_task_completed",
            "orchestrate_id": "orch-1",
            "id": "code",
            "agent": "explore",
            "cycles": 1,
            "tool_calls": 1,
            "input_tokens": 11,
            "output_tokens": 7,
            "duration_ms": 250,
            "result_excerpt": "Code structure summarized",
        }),
    ));

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed = (0..8)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replayed event should arrive before timeout")
                .expect("replay should produce serialized event");
            serde_json::from_str::<serde_json::Value>(&raw)
                .expect("replayed event should be valid json")
        })
        .collect::<Vec<_>>();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[1]["type"], "orchestrate_started");
    assert_eq!(replayed[1]["orchestrate_id"], "orch-1");
    assert_eq!(
        replayed[1]["tasks"][0]["prompt_preview"],
        "Analyze codebase structure"
    );
    assert_eq!(replayed[2]["type"], "orchestrate_layer");
    assert_eq!(replayed[2]["layer"], 1);
    assert_eq!(replayed[3]["type"], "orchestrate_task_started");
    assert_eq!(replayed[3]["id"], "code");
    assert_eq!(replayed[4]["type"], "orchestrate_task_started");
    assert_eq!(replayed[4]["orchestrate_id"], "orch-1");
    assert_eq!(replayed[4]["id"], "docs");
    assert_eq!(replayed[5]["type"], "task_progress");
    assert_eq!(replayed[5]["task_id"], "orch-1:docs");
    assert_eq!(replayed[6]["type"], "task_tool");
    assert_eq!(replayed[6]["task_id"], "orch-1:docs");
    assert_eq!(replayed[7]["type"], "orchestrate_task_completed");
    assert_eq!(replayed[7]["id"], "code");
    assert_eq!(replayed[7]["result_excerpt"], "Code structure summarized");
}

#[test]
fn replay_preserves_completed_standalone_task_until_round_ends() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-task-done-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"task_started","task_id":"t-1","agent":"coder","prompt":"Do stuff"}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"task_tool",
            "task_id":"t-1",
            "agent":"coder",
            "tool":"read_file",
            "id":"tl-1",
            "arguments":"{\"path\":\"README.md\"}"
        }),
    ));
    // Task completes —should still be replayable until round "done"
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"task_completed","task_id":"t-1","agent":"coder",
            "cycles":2,"tool_calls":1,"duration_ms":500,
            "result_excerpt":"Delegated analysis complete"
        }),
    ));

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed: Vec<serde_json::Value> = (0..4)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replay event should arrive before timeout")
                .expect("replay should produce event");
            serde_json::from_str(&raw).expect("valid json")
        })
        .collect();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[1]["type"], "task_started");
    assert_eq!(replayed[1]["task_id"], "t-1");
    assert_eq!(replayed[2]["type"], "task_tool");
    assert_eq!(replayed[2]["arguments"], "{\"path\":\"README.md\"}");
    assert_eq!(replayed[3]["type"], "task_completed");
    assert_eq!(replayed[3]["task_id"], "t-1");
    assert_eq!(replayed[3]["result_excerpt"], "Delegated analysis complete");
}

#[test]
fn replay_preserves_completed_orchestration_until_round_ends() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-orch-done-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_started",
            "orchestrate_id":"orch-2",
            "task_count":2,
            "layer_count":1,
            "tasks":[
                {"id":"a","agent":"explore","depends_on":[]},
                {"id":"b","agent":"coder","depends_on":[]}
            ],
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_task_started",
            "orchestrate_id":"orch-2","id":"a","agent":"explore",
            "prompt":"Analyze",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_task_completed",
            "orchestrate_id":"orch-2","id":"a","agent":"explore",
            "cycles":1,"tool_calls":0,"duration_ms":100,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_task_started",
            "orchestrate_id":"orch-2","id":"b","agent":"coder",
            "prompt":"Code",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_task_completed",
            "orchestrate_id":"orch-2","id":"b","agent":"coder",
            "cycles":2,"tool_calls":3,"duration_ms":400,
        }),
    ));
    // Orchestration completes —should still be replayable until round "done"
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_completed",
            "orchestrate_id":"orch-2","task_count":2,"duration_ms":500,
        }),
    ));

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed: Vec<serde_json::Value> = (0..7)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replay event should arrive before timeout")
                .expect("replay should produce event");
            serde_json::from_str(&raw).expect("valid json")
        })
        .collect();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[1]["type"], "orchestrate_started");
    assert_eq!(replayed[1]["orchestrate_id"], "orch-2");
    assert_eq!(replayed[2]["type"], "orchestrate_task_started");
    assert_eq!(replayed[2]["id"], "a");
    assert_eq!(replayed[3]["type"], "orchestrate_task_completed");
    assert_eq!(replayed[3]["id"], "a");
    assert_eq!(replayed[4]["type"], "orchestrate_task_started");
    assert_eq!(replayed[4]["id"], "b");
    assert_eq!(replayed[5]["type"], "orchestrate_task_completed");
    assert_eq!(replayed[5]["id"], "b");
    assert_eq!(replayed[6]["type"], "orchestrate_completed");
    assert_eq!(replayed[6]["orchestrate_id"], "orch-2");
}

#[test]
fn replay_preserves_multiple_orchestrations_until_round_ends() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-orch-multi-done-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_started",
            "orchestrate_id":"orch-1",
            "task_count":1,
            "layer_count":1,
            "tasks":[
                {"id":"a","agent":"explore","depends_on":[]}
            ],
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_task_started",
            "orchestrate_id":"orch-1",
            "id":"a",
            "agent":"explore",
            "prompt":"Explore code",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_task_completed",
            "orchestrate_id":"orch-1",
            "id":"a",
            "agent":"explore",
            "cycles":1,
            "tool_calls":0,
            "duration_ms":100,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_completed",
            "orchestrate_id":"orch-1",
            "task_count":1,
            "duration_ms":120,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_started",
            "orchestrate_id":"orch-2",
            "task_count":1,
            "layer_count":1,
            "tasks":[
                {"id":"b","agent":"coder","depends_on":[]}
            ],
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_task_started",
            "orchestrate_id":"orch-2",
            "id":"b",
            "agent":"coder",
            "prompt":"Write code",
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_task_completed",
            "orchestrate_id":"orch-2",
            "id":"b",
            "agent":"coder",
            "cycles":2,
            "tool_calls":1,
            "duration_ms":200,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_completed",
            "orchestrate_id":"orch-2",
            "task_count":1,
            "duration_ms":220,
        }),
    ));

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed: Vec<serde_json::Value> = (0..9)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replay event should arrive before timeout")
                .expect("replay should produce event");
            serde_json::from_str(&raw).expect("valid json")
        })
        .collect();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[1]["type"], "orchestrate_started");
    assert_eq!(replayed[1]["orchestrate_id"], "orch-1");
    assert_eq!(replayed[2]["type"], "orchestrate_task_started");
    assert_eq!(replayed[2]["id"], "a");
    assert_eq!(replayed[3]["type"], "orchestrate_task_completed");
    assert_eq!(replayed[3]["id"], "a");
    assert_eq!(replayed[4]["type"], "orchestrate_completed");
    assert_eq!(replayed[4]["orchestrate_id"], "orch-1");
    assert_eq!(replayed[5]["type"], "orchestrate_started");
    assert_eq!(replayed[5]["orchestrate_id"], "orch-2");
    assert_eq!(replayed[6]["type"], "orchestrate_task_started");
    assert_eq!(replayed[6]["id"], "b");
    assert_eq!(replayed[7]["type"], "orchestrate_task_completed");
    assert_eq!(replayed[7]["id"], "b");
    assert_eq!(replayed[8]["type"], "orchestrate_completed");
    assert_eq!(replayed[8]["orchestrate_id"], "orch-2");
    assert!(matches!(
        replay_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[test]
fn replay_preserves_task_and_orchestration_global_order() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-task-orch-order-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(16);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"task_started","task_id":"t-1","agent":"coder","prompt":"Do stuff"}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"task_tool","task_id":"t-1","agent":"coder","tool":"read_file","id":"tl-1"}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"task_completed",
            "task_id":"t-1",
            "agent":"coder",
            "cycles":2,
            "tool_calls":1,
            "duration_ms":500,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_started",
            "orchestrate_id":"orch-mixed",
            "task_count":1,
            "layer_count":1,
            "tasks":[
                {"id":"a","agent":"explore","depends_on":[]}
            ],
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"orchestrate_task_started",
            "orchestrate_id":"orch-mixed",
            "id":"a",
            "agent":"explore",
            "prompt":"Analyze code",
        }),
    ));

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed: Vec<serde_json::Value> = (0..6)
        .map(|_| {
            let raw = rt
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(2), replay_rx.recv()).await
                })
                .expect("replay event should arrive before timeout")
                .expect("replay should produce event");
            serde_json::from_str(&raw).expect("valid json")
        })
        .collect();

    assert_eq!(replayed[0]["type"], "start");
    assert_eq!(replayed[1]["type"], "task_started");
    assert_eq!(replayed[1]["task_id"], "t-1");
    assert_eq!(replayed[2]["type"], "task_tool");
    assert_eq!(replayed[2]["id"], "tl-1");
    assert_eq!(replayed[3]["type"], "task_completed");
    assert_eq!(replayed[3]["task_id"], "t-1");
    assert_eq!(replayed[4]["type"], "orchestrate_started");
    assert_eq!(replayed[4]["orchestrate_id"], "orch-mixed");
    assert_eq!(replayed[5]["type"], "orchestrate_task_started");
    assert_eq!(replayed[5]["id"], "a");
    assert!(matches!(
        replay_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
}

#[test]
fn dispatch_live_event_ignores_stale_connection_after_rebind() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-rebind-{}", now_epoch());
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(4);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        2,
        &bound_tx,
        true,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 1,
            "phase": "analyze",
            "cycle": 1,
            "react_visible": true,
        }),
    ));

    assert!(
        rt.block_on(state.live_rounds.lock())
            .get(&session_id)
            .is_none()
    );
    assert!(bound_rx.try_recv().is_err());

    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        2,
        json!({
            "type": "start",
            "round": 1,
            "phase": "analyze",
            "cycle": 1,
            "react_visible": true,
        }),
    ));

    let payload = rt
        .block_on(bound_rx.recv())
        .expect("current binding should receive live event");
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("payload should be valid json");
    assert_eq!(parsed["type"].as_str(), Some("start"));
    assert!(
        rt.block_on(state.live_rounds.lock())
            .get(&session_id)
            .is_some()
    );
}

#[test]
fn best_effort_tool_output_preserves_replay_when_writer_queue_is_full() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-tool-output-backpressure-{}", now_epoch());
    let writer_capacity = 4;
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(writer_capacity);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        true,
    ));
    rt.block_on(dispatch_live_event(
        state.as_ref(),
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(dispatch_live_event(
        state.as_ref(),
        &session_id,
        1,
        json!({
            "type":"tool_call",
            "id":"tool-1",
            "name":"exec",
            "arguments":"{\"command\":\"echo hi\"}",
        }),
    ));

    rt.block_on(async {
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
    });

    rt.block_on(async {
        let _ = bound_rx
            .recv()
            .await
            .expect("start event should have been queued");
        let _ = bound_rx
            .recv()
            .await
            .expect("tool_call event should have been queued");
        for _ in 0..writer_capacity {
            bound_tx
                .send(json!({"type":"sentinel"}).to_string())
                .await
                .expect("sentinel should fill writer queue");
        }
    });

    let (dummy_live_tx, _dummy_live_rx) =
        mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let replay_ctx = LiveOutputReplayCtx {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };
    rt.block_on(forward_tool_output_event_best_effort(
        &dummy_live_tx,
        json!({
            "type":"tool_output",
            "id":"tool-1",
            "name":"exec",
            "stream":"stdout",
            "chunk":"queued output",
        }),
        Some(&replay_ctx),
    ));

    for _ in 0..writer_capacity {
        assert_eq!(
            bound_rx
                .try_recv()
                .expect("only the queued sentinels should remain"),
            json!({"type":"sentinel"}).to_string()
        );
    }
    assert!(matches!(
        bound_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    rt.block_on(async {
        let live_rounds = state.live_rounds.lock().await;
        let round = live_rounds
            .get(&session_id)
            .expect("live round should remain available for replay");
        let tool = round
            .tools
            .iter()
            .find(|tool| tool.id == "tool-1")
            .expect("tool replay state should exist");
        assert_eq!(tool.live_output, "queued output");
    });
}

#[test]
fn best_effort_tool_output_disconnects_slow_client_when_writer_queue_stays_full() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-tool-output-retry-{}", now_epoch());
    let writer_capacity = 2;
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(writer_capacity);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        true,
    ));
    rt.block_on(dispatch_live_event(
        state.as_ref(),
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(dispatch_live_event(
        state.as_ref(),
        &session_id,
        1,
        json!({
            "type":"tool_call",
            "id":"tool-1",
            "name":"exec",
            "arguments":"{\"command\":\"echo hi\"}",
        }),
    ));

    rt.block_on(async {
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
        let _ = bound_rx.recv().await.expect("start should be queued");
        let _ = bound_rx.recv().await.expect("tool_call should be queued");
        for _ in 0..writer_capacity {
            bound_tx
                .send(json!({"type":"sentinel"}).to_string())
                .await
                .expect("sentinel should fill the writer queue");
        }
    });

    let (dummy_live_tx, _dummy_live_rx) =
        mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let replay_ctx = LiveOutputReplayCtx {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };
    rt.block_on(forward_tool_output_event_best_effort(
        &dummy_live_tx,
        json!({
            "type":"tool_output",
            "id":"tool-1",
            "name":"exec",
            "stream":"stdout",
            "chunk":"queued output",
        }),
        Some(&replay_ctx),
    ));

    for _ in 0..writer_capacity {
        assert_eq!(
            bound_rx
                .try_recv()
                .expect("sentinel should still be queued"),
            json!({"type":"sentinel"}).to_string()
        );
    }
    assert!(matches!(
        bound_rx.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    rt.block_on(async {
        let clients = state.session_clients.lock().await;
        assert!(
            !clients.contains_key(&session_id),
            "persistently full writer queue should disconnect the slow client"
        );
    });
}

#[test]
fn live_dispatch_serializes_normal_events_after_recovered_tool_output_flush() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-tool-output-serialize-{}", now_epoch());
    let writer_capacity = 2;
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(writer_capacity);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        true,
    ));
    rt.block_on(dispatch_live_event(
        state.as_ref(),
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(dispatch_live_event(
        state.as_ref(),
        &session_id,
        1,
        json!({
            "type":"tool_call",
            "id":"tool-1",
            "name":"exec",
            "arguments":"{\"command\":\"echo hi\"}",
        }),
    ));

    rt.block_on(async {
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
        let _ = bound_rx.recv().await.expect("start should be queued");
        let _ = bound_rx.recv().await.expect("tool_call should be queued");
        for _ in 0..writer_capacity {
            bound_tx
                .send(json!({"type":"sentinel"}).to_string())
                .await
                .expect("sentinel should fill the writer queue");
        }
    });

    let (dummy_live_tx, _dummy_live_rx) =
        mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let replay_ctx = LiveOutputReplayCtx {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };
    let forward = rt.block_on(async {
        tokio::spawn(async move {
            forward_tool_output_event_best_effort(
                &dummy_live_tx,
                json!({
                    "type":"tool_output",
                    "id":"tool-1",
                    "name":"exec",
                    "stream":"stdout",
                    "chunk":"queued output",
                }),
                Some(&replay_ctx),
            )
            .await;
        })
    });

    for _ in 0..writer_capacity {
        let _ = rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
                .await
                .expect("sentinel should arrive before timeout")
                .expect("sentinel should be delivered")
        });
    }
    rt.block_on(async {
        forward.await.expect("forward task should complete");
    });

    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"tool_result",
            "id":"tool-1",
            "name":"exec",
            "result":"done",
            "duration_ms":123,
        }),
    ));

    let flushed_output = serde_json::from_str::<serde_json::Value>(
        &bound_rx
            .try_recv()
            .expect("queued tool output should flush before tool_result"),
    )
    .expect("flushed payload should be valid json");
    assert_eq!(flushed_output["type"], "tool_output");
    assert_eq!(flushed_output["chunk"], "queued output");

    let tool_result = serde_json::from_str::<serde_json::Value>(
        &bound_rx
            .try_recv()
            .expect("tool_result should follow flushed output"),
    )
    .expect("tool_result payload should be valid json");
    assert_eq!(tool_result["type"], "tool_result");

    rt.block_on(async {
        let clients = state.session_clients.lock().await;
        let binding = clients
            .get(&session_id)
            .expect("session client binding should exist");
        assert!(binding.pending_events.is_empty());
        assert!(!binding.live_send_in_progress);
    });
}

#[test]
fn best_effort_subagent_tool_output_preserves_orchestration_context_for_replay() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-subagent-tool-output-orch-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(8);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(async {
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
    });

    let (dummy_live_tx, _dummy_live_rx) =
        mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let replay_ctx = LiveOutputReplayCtx {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };
    rt.block_on(forward_tool_output_event_best_effort(
        &dummy_live_tx,
        json!({
            "type":"tool_output",
            "task_id":"orch-1:task-1",
            "subagent":"agent-1",
            "id":"tool-1",
            "name":"exec",
            "stream":"stdout",
            "chunk":"delegated output",
        }),
        Some(&replay_ctx),
    ));

    rt.block_on(async {
        let clients = state.session_clients.lock().await;
        let binding = clients
            .get(&session_id)
            .expect("session client binding should exist");
        assert_eq!(binding.pending_events.len(), 4);
        let queued = binding.pending_events.iter().cloned().collect::<Vec<_>>();
        assert_eq!(queued[1]["type"], "orchestrate_started");
        assert_eq!(queued[1]["orchestrate_id"], "orch-1");
        assert_eq!(queued[1]["synthetic"], true);
        assert_eq!(queued[2]["type"], "orchestrate_task_started");
        assert_eq!(queued[2]["orchestrate_id"], "orch-1");
        assert_eq!(queued[2]["id"], "task-1");
        assert_eq!(queued[3]["type"], "tool_output");
        assert_eq!(queued[3]["task_id"], "orch-1:task-1");
    });

    rt.block_on(async {
        let live_rounds = state.live_rounds.lock().await;
        let round = live_rounds
            .get(&session_id)
            .expect("live round should exist");
        assert_eq!(round.delegated_events.len(), 3);
        assert_eq!(round.delegated_events[0]["type"], "orchestrate_started");
        assert_eq!(round.delegated_events[0]["orchestrate_id"], "orch-1");
        assert_eq!(round.delegated_events[0]["synthetic"], true);
        assert_eq!(
            round.delegated_events[1]["type"],
            "orchestrate_task_started"
        );
        assert_eq!(round.delegated_events[1]["orchestrate_id"], "orch-1");
        assert_eq!(round.delegated_events[1]["id"], "task-1");
        assert_eq!(round.delegated_events[2]["type"], "tool_output");
    });
}

#[test]
fn synthetic_delegated_task_does_not_mark_active_without_stored_start_event() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-subagent-cap-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(8);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(async {
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
        let mut live_rounds = state.live_rounds.lock().await;
        let round = live_rounds
            .get_mut(&session_id)
            .expect("live round should exist");
        round.delegated_events = (0..DELEGATED_EVENTS_CAP)
            .map(|idx| json!({"type":"task_progress","task_id":format!("seed-{idx}")}))
            .collect();
    });

    let (dummy_live_tx, _dummy_live_rx) =
        mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let replay_ctx = LiveOutputReplayCtx {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };
    rt.block_on(forward_tool_output_event_best_effort(
        &dummy_live_tx,
        json!({
            "type":"tool_output",
            "task_id":"orch-cap:task-1",
            "subagent":"agent-1",
            "id":"tool-1",
            "name":"exec",
            "stream":"stdout",
            "chunk":"delegated output",
        }),
        Some(&replay_ctx),
    ));

    rt.block_on(async {
        let live_rounds = state.live_rounds.lock().await;
        let round = live_rounds
            .get(&session_id)
            .expect("live round should exist");
        assert!(!round.active_tasks.contains("orch-cap:task-1"));
        assert_eq!(round.delegated_events.len(), DELEGATED_EVENTS_CAP);
    });
}

#[test]
fn synthetic_orchestration_keeps_replayable_open_state_when_only_panel_fits() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-subagent-orch-cap-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(8);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(async {
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
        let mut live_rounds = state.live_rounds.lock().await;
        let round = live_rounds
            .get_mut(&session_id)
            .expect("live round should exist");
        round.delegated_events = (0..DELEGATED_EVENTS_CAP - 1)
            .map(|idx| json!({"type":"task_progress","task_id":format!("seed-{idx}")}))
            .collect();
    });

    let (dummy_live_tx, _dummy_live_rx) =
        mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let replay_ctx = LiveOutputReplayCtx {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };
    rt.block_on(forward_tool_output_event_best_effort(
        &dummy_live_tx,
        json!({
            "type":"tool_output",
            "task_id":"orch-tight:task-1",
            "subagent":"agent-1",
            "id":"tool-1",
            "name":"exec",
            "stream":"stdout",
            "chunk":"delegated output",
        }),
        Some(&replay_ctx),
    ));

    rt.block_on(async {
        let live_rounds = state.live_rounds.lock().await;
        let round = live_rounds
            .get(&session_id)
            .expect("live round should exist");
        assert!(round.active_orchestrations.contains("orch-tight"));
        assert!(round.active_tasks.contains("orch-tight:task-1"));
        assert_eq!(round.delegated_events.len(), DELEGATED_EVENTS_CAP);
        assert_eq!(
            round.delegated_events[DELEGATED_EVENTS_CAP - 1]["type"],
            "orchestrate_started"
        );
        assert_eq!(
            round.delegated_events[DELEGATED_EVENTS_CAP - 1]["orchestrate_id"],
            "orch-tight"
        );
        assert_eq!(
            round.delegated_events[DELEGATED_EVENTS_CAP - 1]["tasks"][0]["id"],
            "task-1"
        );
    });
}

#[test]
fn best_effort_tool_output_preserves_order_after_writer_queue_recovers() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-tool-output-flush-{}", now_epoch());
    let writer_capacity = 2;
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(writer_capacity);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        true,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"tool_call",
            "id":"tool-1",
            "name":"exec",
            "arguments":"{\"command\":\"echo hi\"}",
        }),
    ));

    rt.block_on(async {
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
        let _ = bound_rx.recv().await.expect("start should be queued");
        let _ = bound_rx.recv().await.expect("tool_call should be queued");
        for _ in 0..writer_capacity {
            bound_tx
                .send(json!({"type":"sentinel"}).to_string())
                .await
                .expect("sentinel should fill the writer queue");
        }
    });

    let (dummy_live_tx, _dummy_live_rx) =
        mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let replay_ctx = LiveOutputReplayCtx {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };
    let forward = rt.block_on(async {
        tokio::spawn(async move {
            forward_tool_output_event_best_effort(
                &dummy_live_tx,
                json!({
                    "type":"tool_output",
                    "id":"tool-1",
                    "name":"exec",
                    "stream":"stdout",
                    "chunk":"queued output",
                }),
                Some(&replay_ctx),
            )
            .await;
        })
    });

    for _ in 0..writer_capacity {
        assert_eq!(
            rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
                    .await
                    .expect("sentinel should arrive before timeout")
                    .expect("sentinel should be delivered")
            }),
            json!({"type":"sentinel"}).to_string()
        );
    }
    rt.block_on(async {
        forward.await.expect("forward task should complete");
        let clients = state.session_clients.lock().await;
        let binding = clients
            .get(&session_id)
            .expect("session client binding should exist");
        assert!(binding.pending_events.is_empty());
        assert!(!binding.live_send_in_progress);
    });

    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type":"tool_progress",
            "id":"tool-1",
            "name":"exec",
            "elapsed_ms":123,
        }),
    ));

    let flushed_output = serde_json::from_str::<serde_json::Value>(
        &bound_rx
            .try_recv()
            .expect("queued tool output should flush before current event"),
    )
    .expect("flushed payload should be valid json");
    assert_eq!(flushed_output["type"], "tool_output");
    assert_eq!(flushed_output["chunk"], "queued output");

    let current_event = serde_json::from_str::<serde_json::Value>(
        &bound_rx
            .try_recv()
            .expect("current event should follow the flushed backlog"),
    )
    .expect("current payload should be valid json");
    assert_eq!(current_event["type"], "tool_progress");
}

#[test]
fn best_effort_tool_output_flushes_after_writer_queue_recovers_without_followup_event() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-tool-output-drain-{}", now_epoch());
    let writer_capacity = 1;
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(writer_capacity);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        true,
    ));
    rt.block_on(dispatch_live_event(
        state.as_ref(),
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));

    rt.block_on(async {
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
        let _ = bound_rx.recv().await.expect("start should be queued");
        bound_tx
            .send(json!({"type":"sentinel"}).to_string())
            .await
            .expect("sentinel should fill the writer queue");
    });

    let (dummy_live_tx, _dummy_live_rx) =
        mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let replay_ctx = LiveOutputReplayCtx {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };

    let forward = rt.block_on(async {
        let state = Arc::clone(&state);
        let session_id = session_id.clone();
        tokio::spawn(async move {
            forward_tool_output_event_best_effort(
                &dummy_live_tx,
                json!({
                    "type":"tool_output",
                    "id":"tool-1",
                    "name":"exec",
                    "stream":"stdout",
                    "chunk":"queued output",
                }),
                Some(&replay_ctx),
            )
            .await;
            let clients = state.session_clients.lock().await;
            let binding = clients
                .get(&session_id)
                .expect("session client binding should exist");
            assert!(binding.pending_events.is_empty());
            assert!(!binding.live_send_in_progress);
        })
    });

    let sentinel = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("sentinel should arrive before timeout")
            .expect("sentinel should be delivered")
    });
    assert_eq!(sentinel, json!({"type":"sentinel"}).to_string());

    let flushed_tool_call = serde_json::from_str::<serde_json::Value>(&rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("queued tool call should arrive before timeout")
            .expect("queued tool call should be delivered")
    }))
    .expect("flushed tool call should be valid json");
    assert_eq!(flushed_tool_call["type"], "tool_call");
    assert_eq!(flushed_tool_call["id"], "tool-1");

    let flushed_output = serde_json::from_str::<serde_json::Value>(&rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("queued tool output should arrive before timeout")
            .expect("queued tool output should be delivered")
    }))
    .expect("flushed payload should be valid json");
    assert_eq!(flushed_output["type"], "tool_output");
    assert_eq!(flushed_output["chunk"], "queued output");

    rt.block_on(async {
        forward.await.expect("forward task should complete");
    });
}

#[test]
fn best_effort_subagent_tool_output_synthesizes_task_started_for_replay() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-subagent-tool-output-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(4);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(async {
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
    });

    let (dummy_live_tx, _dummy_live_rx) =
        mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let replay_ctx = LiveOutputReplayCtx {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };
    rt.block_on(forward_tool_output_event_best_effort(
        &dummy_live_tx,
        json!({
            "type":"tool_output",
            "task_id":"task-1",
            "subagent":"agent-1",
            "id":"tool-1",
            "name":"exec",
            "stream":"stdout",
            "chunk":"delegated output",
        }),
        Some(&replay_ctx),
    ));

    rt.block_on(async {
        let clients = state.session_clients.lock().await;
        let binding = clients
            .get(&session_id)
            .expect("session client binding should exist");
        assert_eq!(binding.pending_events.len(), 3);
        let queued = binding.pending_events.iter().cloned().collect::<Vec<_>>();
        assert_eq!(queued[1]["type"], "task_started");
        assert_eq!(queued[1]["task_id"], "task-1");
        assert_eq!(queued[2]["type"], "tool_output");
        assert_eq!(queued[2]["chunk"], "delegated output");
    });

    rt.block_on(async {
        let live_rounds = state.live_rounds.lock().await;
        let round = live_rounds
            .get(&session_id)
            .expect("live round should exist");
        assert_eq!(round.delegated_events.len(), 2);
        assert_eq!(round.delegated_events[0]["type"], "task_started");
        assert_eq!(round.delegated_events[1]["type"], "tool_output");
    });
}

#[test]
fn synthetic_orchestration_growth_preserves_existing_task_agents() {
    let first = json!({
        "type":"tool_output",
        "task_id":"orch-1:task-a",
        "subagent":"agent-1",
        "id":"tool-1",
        "name":"exec",
        "stream":"stdout",
        "chunk":"output a",
    });
    let second = json!({
        "type":"tool_output",
        "task_id":"orch-1:task-b",
        "subagent":"agent-2",
        "id":"tool-2",
        "name":"exec",
        "stream":"stdout",
        "chunk":"output b",
    });
    let delegated_events = vec![
        json!({
            "type":"orchestrate_started",
            "orchestrate_id":"orch-1",
            "task_count":1,
            "layer_count":1,
            "tasks":[{
                "id":"task-a",
                "agent":"agent-1",
                "depends_on":[],
                "prompt_preview":"",
            }],
            "synthetic":true,
        }),
        json!({
            "type":"task_started",
            "task_id":"orch-1:task-a",
            "agent":"agent-1",
            "prompt":"",
        }),
        first,
    ];
    let active_tasks = HashSet::from(["orch-1:task-a".to_string(), "orch-1:task-b".to_string()]);

    let synthetic = synthetic_orchestrate_started_event_for_output(
        &second,
        &delegated_events,
        Some(&active_tasks),
    )
    .expect("synthetic orchestrate_started should be generated");

    let tasks = synthetic["tasks"]
        .as_array()
        .expect("synthetic tasks should be an array");
    let task_a = tasks
        .iter()
        .find(|task| task["id"] == "task-a")
        .expect("task-a should be present");
    let task_b = tasks
        .iter()
        .find(|task| task["id"] == "task-b")
        .expect("task-b should be present");
    assert_eq!(task_a["agent"], "agent-1");
    assert_eq!(task_b["agent"], "agent-2");
}

#[test]
fn best_effort_subagent_tool_output_replays_updated_synthetic_orchestration_to_client() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-synthetic-orchestrate-client-grow-{}", now_epoch());
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(8);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        true,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(async {
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
    });

    let (dummy_live_tx, _dummy_live_rx) =
        mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let replay_ctx = LiveOutputReplayCtx {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };

    rt.block_on(forward_tool_output_event_best_effort(
        &dummy_live_tx,
        json!({
            "type":"tool_output",
            "task_id":"orch-1:task-a",
            "subagent":"agent-1",
            "id":"tool-1",
            "name":"exec",
            "stream":"stdout",
            "chunk":"output a",
        }),
        Some(&replay_ctx),
    ));
    rt.block_on(forward_tool_output_event_best_effort(
        &dummy_live_tx,
        json!({
            "type":"tool_output",
            "task_id":"orch-1:task-b",
            "subagent":"agent-2",
            "id":"tool-2",
            "name":"exec",
            "stream":"stdout",
            "chunk":"output b",
        }),
        Some(&replay_ctx),
    ));

    let start = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("start event should arrive before timeout")
            .expect("start event should be queued")
    });
    let first_orchestrate_started = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("first synthetic orchestrate_started should arrive before timeout")
            .expect("first synthetic orchestrate_started should be queued")
    });
    let task_a_started = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("first synthetic task_started should arrive before timeout")
            .expect("first synthetic task_started should be queued")
    });
    let task_a_output = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("first tool_output should arrive before timeout")
            .expect("first tool_output should be queued")
    });
    let second_orchestrate_started = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("updated synthetic orchestrate_started should arrive before timeout")
            .expect("updated synthetic orchestrate_started should be queued")
    });
    let task_b_started = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("second synthetic task_started should arrive before timeout")
            .expect("second synthetic task_started should be queued")
    });
    let task_b_output = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), bound_rx.recv())
            .await
            .expect("second tool_output should arrive before timeout")
            .expect("second tool_output should be queued")
    });

    let start: serde_json::Value = serde_json::from_str(&start).expect("start should be json");
    let first_orchestrate_started: serde_json::Value =
        serde_json::from_str(&first_orchestrate_started)
            .expect("first orchestrate_started should be json");
    let task_a_started: serde_json::Value =
        serde_json::from_str(&task_a_started).expect("task-a start should be json");
    let task_a_output: serde_json::Value =
        serde_json::from_str(&task_a_output).expect("task-a output should be json");
    let second_orchestrate_started: serde_json::Value =
        serde_json::from_str(&second_orchestrate_started)
            .expect("second orchestrate_started should be json");
    let task_b_started: serde_json::Value =
        serde_json::from_str(&task_b_started).expect("task-b start should be json");
    let task_b_output: serde_json::Value =
        serde_json::from_str(&task_b_output).expect("task-b output should be json");

    assert_eq!(start["type"], "start");
    assert_eq!(first_orchestrate_started["type"], "orchestrate_started");
    assert_eq!(first_orchestrate_started["task_count"], 1);
    assert_eq!(task_a_started["type"], "orchestrate_task_started");
    assert_eq!(task_a_started["orchestrate_id"], "orch-1");
    assert_eq!(task_a_started["id"], "task-a");
    assert_eq!(task_a_output["type"], "tool_output");
    assert_eq!(second_orchestrate_started["type"], "orchestrate_started");
    assert_eq!(second_orchestrate_started["task_count"], 2);
    let tasks = second_orchestrate_started["tasks"]
        .as_array()
        .expect("updated synthetic tasks should be an array");
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().any(|task| task["id"] == "task-a"));
    assert!(tasks.iter().any(|task| task["id"] == "task-b"));
    assert_eq!(task_b_started["type"], "orchestrate_task_started");
    assert_eq!(task_b_started["orchestrate_id"], "orch-1");
    assert_eq!(task_b_started["id"], "task-b");
    assert_eq!(task_b_output["type"], "tool_output");
    assert_eq!(task_b_output["task_id"], "orch-1:task-b");
}

#[test]
fn best_effort_subagent_tool_output_grows_synthetic_orchestration_for_new_tasks() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let session_id = format!("live-synthetic-orchestrate-grow-{}", now_epoch());
    let (bound_tx, _bound_rx) = mpsc::channel::<String>(8);

    rt.block_on(bind_session_connection(
        state.as_ref(),
        &session_id,
        1,
        &bound_tx,
        false,
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({"type":"start","round":1,"phase":"act","cycle":1,"react_visible":true}),
    ));
    rt.block_on(async {
        state.active_runs.lock().await.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: CancellationToken::new(),
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
    });

    let (dummy_live_tx, _dummy_live_rx) =
        mpsc::channel::<serde_json::Value>(LIVE_EVENT_CHANNEL_CAPACITY);
    let replay_ctx = LiveOutputReplayCtx {
        state: Arc::clone(&state),
        session_id: session_id.clone(),
    };

    rt.block_on(forward_tool_output_event_best_effort(
        &dummy_live_tx,
        json!({
            "type":"tool_output",
            "task_id":"orch-1:task-a",
            "subagent":"agent-1",
            "id":"tool-1",
            "name":"exec",
            "stream":"stdout",
            "chunk":"output a",
        }),
        Some(&replay_ctx),
    ));
    rt.block_on(forward_tool_output_event_best_effort(
        &dummy_live_tx,
        json!({
            "type":"tool_output",
            "task_id":"orch-1:task-b",
            "subagent":"agent-2",
            "id":"tool-2",
            "name":"exec",
            "stream":"stdout",
            "chunk":"output b",
        }),
        Some(&replay_ctx),
    ));

    rt.block_on(async {
        let live_rounds = state.live_rounds.lock().await;
        let round = live_rounds
            .get(&session_id)
            .expect("live round should exist");
        let synthetic = round
            .delegated_events
            .iter()
            .find(|event| event["type"] == "orchestrate_started")
            .expect("synthetic orchestrate_started should exist");
        let tasks = synthetic["tasks"]
            .as_array()
            .expect("synthetic tasks should be an array");
        assert_eq!(tasks.len(), 2);
        assert_eq!(synthetic["task_count"], 2);
        assert!(tasks.iter().any(|task| task["id"] == "task-a"));
        assert!(tasks.iter().any(|task| task["id"] == "task-b"));
    });
}

#[test]
fn dispatch_live_event_allows_active_run_source_after_rebind() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-run-rebind-{}", now_epoch());
    let run_cancel = CancellationToken::new();
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(4);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        2,
        &bound_tx,
        true,
    ));
    {
        let mut runs = rt.block_on(state.active_runs.lock());
        runs.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: run_cancel,
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
    }

    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "start",
            "round": 1,
            "phase": "analyze",
            "cycle": 1,
            "react_visible": true,
        }),
    ));

    let payload = rt
        .block_on(bound_rx.recv())
        .expect("rebound client should receive live event from active run source");
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("payload should be valid json");
    assert_eq!(parsed["type"].as_str(), Some("start"));

    let live_rounds = rt.block_on(state.live_rounds.lock());
    let round = live_rounds
        .get(&session_id)
        .expect("live round should be recorded");
    assert_eq!(round.connection_id, 1);
}

#[test]
fn dispatch_live_event_routes_non_tool_output_updates_to_rebound_client() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-run-rebind-delta-{}", now_epoch());
    let run_cancel = CancellationToken::new();
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(4);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        2,
        &bound_tx,
        true,
    ));
    {
        let mut runs = rt.block_on(state.active_runs.lock());
        runs.insert(
            session_id.clone(),
            SessionRunBinding {
                connection_id: 1,
                cancel: run_cancel,
                stop_requested: Arc::new(AtomicBool::new(false)),
                deferred_interventions: Arc::new(Mutex::new(DeferredInterventionState::open())),
            },
        );
    }

    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "delta",
            "content": "rebound update",
        }),
    ));

    let payload = rt
        .block_on(bound_rx.recv())
        .expect("rebound client should receive delta from active run source");
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("payload should be valid json");
    assert_eq!(parsed["type"].as_str(), Some("delta"));
    assert_eq!(parsed["content"].as_str(), Some("rebound update"));
}

#[test]
fn dispatch_live_event_allows_live_round_source_after_run_teardown() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = test_app_state();
    let session_id = format!("live-round-teardown-{}", now_epoch());
    let (bound_tx, mut bound_rx) = mpsc::channel::<String>(4);

    rt.block_on(bind_session_connection(
        &state,
        &session_id,
        2,
        &bound_tx,
        true,
    ));
    {
        let mut live_rounds = rt.block_on(state.live_rounds.lock());
        live_rounds.insert(
            session_id.clone(),
            LiveRoundState {
                connection_id: 1,
                round: 1,
                react_visible: true,
                phase: Some("finish".into()),
                cycle: Some(1),
                effective_model: None,
                effective_think: None,
                auto_observation_strength: None,
                auto_stagnation_streak: None,
                auto_error_streak: None,
                auto_task_pressure: None,
                auto_action_oriented: None,
                auto_ready_to_finish: None,
                auto_has_blocking_uncertainty: None,
                latest_auto_trace: None,
                latest_compression: LiveCompressionState::default(),
                has_pending_pre_start_context_updates: false,
                has_observation: false,
                assistant_text: String::new(),
                reasoning_text: String::new(),
                reasoning_done: false,
                tools: Vec::new(),
                delegated_events: Vec::new(),
                active_tasks: HashSet::new(),
                active_orchestrations: HashSet::new(),
            },
        );
    }

    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        1,
        json!({
            "type": "done",
            "phase": "complete",
        }),
    ));

    let payload = rt
        .block_on(bound_rx.recv())
        .expect("rebound client should receive terminal event from live round source");
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("payload should be valid json");
    assert_eq!(parsed["type"].as_str(), Some("done"));
    assert!(
        rt.block_on(state.live_rounds.lock())
            .get(&session_id)
            .is_none()
    );
}

// ── Phase 4: Tool Protocol + Session Recovery ────────────────────────────────

#[test]
fn session_version_defaults_to_zero_for_old_sessions() {
    let json_str = r#"{
        "id": "legacy",
        "name": "Legacy",
        "messages": [],
        "created_at": 0,
        "updated_at": 0,
        "tool_calls_count": 0
    }"#;
    let session: Session = serde_json::from_str(json_str).unwrap();
    assert_eq!(session.version, 0);
}

#[test]
fn session_version_is_preserved_in_serialization() {
    let json_str = r#"{
        "id": "v1",
        "name": "V1",
        "messages": [],
        "created_at": 0,
        "updated_at": 0,
        "tool_calls_count": 0,
        "version": 1
    }"#;
    let session: Session = serde_json::from_str(json_str).unwrap();
    assert_eq!(session.version, 1);
    let serialized = serde_json::to_string(&session).unwrap();
    assert!(serialized.contains(r#""version":1"#) || serialized.contains(r#""version": 1"#));
}

#[test]
fn tool_outcome_error_detection_by_convention() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    // Unknown tool —is_error
    let outcome = rt.block_on(tools::execute_tool(
        "nonexistent",
        "{}",
        &test_config(),
        &reqwest::Client::new(),
        std::path::Path::new("."),
        None,
    ));
    assert!(outcome.is_error);

    // think tool is never an error
    let outcome = rt.block_on(tools::execute_tool(
        "think",
        r#"{"thought":"test"}"#,
        &test_config(),
        &reqwest::Client::new(),
        std::path::Path::new("."),
        None,
    ));
    assert!(!outcome.is_error);
    assert!(outcome.duration_ms < 1000); // should be near-instant
}

#[test]
fn tool_outcome_does_not_treat_raw_tool_output_as_failure() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let workspace = std::env::temp_dir().join(format!("lingclaw-tool-output-{}", now_epoch()));
    std::fs::create_dir_all(&workspace).expect("workspace should be created");
    std::fs::write(
        workspace.join("notes.txt"),
        "search output: exec error: command not found",
    )
    .expect("file should be written");

    let outcome = rt.block_on(tools::execute_tool(
        "read_file",
        r#"{"path":"notes.txt"}"#,
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        None,
    ));

    assert!(!outcome.is_error);
    assert!(outcome.output.contains("exec error: command not found"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn tool_outcome_parameter_validation() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    // write_file requires both path and content
    let outcome = rt.block_on(tools::execute_tool(
        "write_file",
        r#"{}"#,
        &test_config(),
        &reqwest::Client::new(),
        std::path::Path::new("."),
        None,
    ));
    assert!(outcome.is_error);
    assert!(outcome.output.contains("missing required parameter"));
}

#[test]
fn observation_summary_includes_error_tools() {
    let results = vec![
        agent::ToolResultEntry {
            id: "ok".into(),
            name: "read_file".into(),
            result: "short ok".into(),
            duration_ms: 5,
            is_error: false,
            call_summary: None,
            trace: None,
        },
        agent::ToolResultEntry {
            id: "err".into(),
            name: "exec".into(),
            result: "exec error: command not found".into(),
            duration_ms: 10,
            is_error: true,
            call_summary: None,
            trace: None,
        },
    ];
    let summaries = agent::summarize_observations(&results);
    // Short OK result should NOT be included; error result should be
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].tool_name, "exec");
    assert!(summaries[0].hint.contains("FAILED"));
}

#[test]
fn prune_messages_tracks_removal_count() {
    let mut messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("sys".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("a".repeat(200_000)),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("b".repeat(200_000)),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("latest".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    let before = messages.len();
    prune_messages(&mut messages, 1000); // very small limit —must prune
    let pruned = before - messages.len();
    assert!(pruned > 0, "should have removed at least one turn");
    // System + latest user should remain
    assert_eq!(messages[0].role, "system");
    assert!(messages.last().unwrap().content.as_deref() == Some("latest"));
}
// ───── Phase 5: check_dangerous_command ─────

#[test]
fn dangerous_command_blocks_rm_rf_root() {
    assert!(check_dangerous_command("rm -rf /").is_some());
    assert!(check_dangerous_command("sudo rm -rf / --no-preserve-root").is_some());
    assert!(check_dangerous_command("rm -rf /*").is_some());
}

#[test]
fn dangerous_command_blocks_mkfs_and_dd() {
    assert!(check_dangerous_command("mkfs.ext4 /dev/sda1").is_some());
    assert!(check_dangerous_command("dd if=/dev/zero of=/dev/sda").is_some());
}

#[test]
fn dangerous_command_blocks_fork_bomb_and_dev_overwrite() {
    assert!(check_dangerous_command(":(){ :|:& };:").is_some());
    assert!(check_dangerous_command("echo test > /dev/sda").is_some());
}

#[test]
fn dangerous_command_blocks_windows_destructive_commands() {
    assert!(check_dangerous_command("format c:").is_some());
    assert!(check_dangerous_command("FORMAT C:").is_some()); // case-insensitive
    assert!(check_dangerous_command("del /f /s /q c:\\windows").is_some());
    assert!(check_dangerous_command("rd /s /q c:\\").is_some());
}

#[test]
fn dangerous_command_allows_safe_commands() {
    assert!(check_dangerous_command("ls -la").is_none());
    assert!(check_dangerous_command("cat /dev/null").is_none());
    assert!(check_dangerous_command("echo hello").is_none());
    assert!(check_dangerous_command("cargo build").is_none());
    assert!(check_dangerous_command("rm temp.txt").is_none());
}

#[test]
fn dangerous_command_normalizes_whitespace() {
    // Extra whitespace between tokens should still match.
    assert!(check_dangerous_command("rm  -rf  /").is_some());
    assert!(check_dangerous_command("rm   -rf   /*").is_some());
    assert!(check_dangerous_command("rm\t-rf\t/").is_some());
    assert!(check_dangerous_command("del  /f  /s  /q  c:\\").is_some());
}

#[test]
fn dangerous_command_detects_new_patterns() {
    assert!(check_dangerous_command("rm -rf ~").is_some());
    assert!(check_dangerous_command("chmod -R 777 /").is_some());
    assert!(check_dangerous_command("chown -R root:root /").is_some());
    assert!(check_dangerous_command("reg delete HKLM\\Software").is_some());
    // Workspace-scoped chown to a non-root user should be allowed.
    assert!(check_dangerous_command("chown -R user:group ./dir").is_none());
}

// ───── Phase 5: truncate ─────

#[test]
fn truncate_short_string_unchanged() {
    let s = "hello world";
    assert_eq!(truncate(s, 100), s);
}

#[test]
fn truncate_ascii_at_boundary() {
    let s = "abcdefghij"; // 10 bytes
    let result = truncate(s, 5);
    assert!(result.starts_with("abcde"));
    assert!(result.contains("[truncated at 5 bytes, total 10 bytes]"));
}

#[test]
fn truncate_utf8_multibyte_boundary() {
    let s = "你好世界"; // 12 bytes (3 per char)
    let result = truncate(s, 7); // mid-char boundary
    // Should cut at char boundary <= 7, which is 6 (after first 2 chars)
    assert!(result.starts_with("你好"));
    assert!(result.contains("[truncated at 6 bytes"));
}

#[test]
fn truncate_emoji_boundary() {
    let s = "\u{1F980}\u{1F980}\u{1F980}"; // 12 bytes (4 per emoji)
    let result = truncate(s, 5); // mid-emoji
    assert!(result.starts_with("\u{1F980}"));
    assert!(result.contains("[truncated at 4 bytes"));
}

#[test]
fn truncate_safe_preserves_char_boundary() {
    // CJK: 3 bytes per char. Cutting at 7 must round down to 6.
    let mut s = "你好世".to_string(); // 9 bytes
    truncate_safe(&mut s, 7);
    assert_eq!(s, "你好");

    // Emoji: 4 bytes per char. Cutting at 5 must round down to 4.
    let mut s = "\u{1F980}\u{1F980}".to_string(); // 8 bytes
    truncate_safe(&mut s, 5);
    assert_eq!(s, "\u{1F980}");

    // Already within limit —unchanged.
    let mut s = "hello".to_string();
    truncate_safe(&mut s, 100);
    assert_eq!(s, "hello");
}

#[test]
fn merge_live_tool_output_keeps_latest_tail() {
    let mut output = "A".repeat(LIVE_REPLAY_CAP);
    let tail = "tail-marker".repeat(16);

    merge_live_tool_output(&mut output, None, &tail);

    assert!(output.starts_with("[live output truncated]\n"));
    assert!(output.ends_with(&tail));
    assert!(output.len() <= LIVE_REPLAY_CAP);
}

// ───── Phase 5: format_size ─────

#[test]
fn format_size_bytes() {
    assert_eq!(format_size(0), "0 B");
    assert_eq!(format_size(512), "512 B");
    assert_eq!(format_size(1023), "1023 B");
}

#[test]
fn format_size_kilobytes() {
    assert_eq!(format_size(1024), "1.0 KB");
    assert_eq!(format_size(1536), "1.5 KB");
}

#[test]
fn format_size_megabytes() {
    assert_eq!(format_size(1024 * 1024), "1.0 MB");
    assert_eq!(format_size(2 * 1024 * 1024), "2.0 MB");
}

// ───── Phase 5: matches_glob ─────

#[test]
fn matches_glob_extension_pattern() {
    assert!(matches_glob("main.rs", "*.rs"));
    assert!(!matches_glob("main.py", "*.rs"));
    assert!(matches_glob("deeply.nested.test.rs", "*.rs"));
}

#[test]
fn matches_glob_prefix_pattern() {
    assert!(matches_glob("test_main.rs", "test_*"));
    assert!(!matches_glob("main_test.rs", "test_*"));
}

#[test]
fn matches_glob_exact_match() {
    assert!(matches_glob("Cargo.toml", "Cargo.toml"));
    assert!(!matches_glob("Cargo.lock", "Cargo.toml"));
}

// ───── Phase 5: estimate_tokens / message_token_len ─────

#[test]
fn message_token_len_empty_message() {
    let msg = ChatMessage {
        role: "user".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    // content=0 + tc=0 + overhead=3 = 3
    assert_eq!(message_token_len(&msg), 3);
}

#[test]
fn message_token_len_content_only() {
    let msg = ChatMessage {
        role: "user".into(),
        content: Some("hello world".into()), // 11 chars
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    // content: 11 ASCII bytes / 4 = 2, + overhead 3 = 5
    assert_eq!(message_token_len(&msg), 5);
}

#[test]
fn message_token_len_with_tool_calls() {
    let msg = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "exec".into(),                 // 4
                arguments: r#"{"cmd":"ls"}"#.into(), // 12
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    };
    // content=0, tc: (4+12)/4 = 4, + overhead 3 = 7
    assert_eq!(message_token_len(&msg), 7);
}

#[test]
fn estimate_tokens_sums_messages() {
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("sys".into()), // 3
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()), // 5
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    // "sys": 3/4=0 + 3=3, "hello": 5/4=1 + 3=4, total=7
    assert_eq!(estimate_tokens(&messages), 7);
}

#[test]
fn message_token_len_cjk_aware() {
    // CJK text: 6 Chinese characters = 18 UTF-8 bytes, but ~6 tokens (1 per char)
    let msg = ChatMessage {
        role: "user".into(),
        content: Some("你好世界测试".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    let cjk_estimate = message_token_len(&msg);

    // Same byte-length ASCII text
    let ascii_msg = ChatMessage {
        role: "user".into(),
        content: Some("a".repeat(18)),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    let ascii_estimate = message_token_len(&ascii_msg);

    // CJK should yield more tokens than ASCII for the same byte length,
    // because CJK characters are ~1 char/token vs ~4 bytes/token.
    assert!(
        cjk_estimate > ascii_estimate,
        "CJK ({cjk_estimate}) should be > ASCII ({ascii_estimate}) for same byte length"
    );
}

#[test]
fn provider_aware_estimate_adds_tool_protocol_overhead() {
    let messages = vec![
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: r#"{"cmd":"ls"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("file-a\nfile-b".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
    ];

    let base = estimate_tokens(&messages);
    let openai = estimate_tokens_for_provider(Provider::OpenAI, &messages);
    let anthropic = estimate_tokens_for_provider(Provider::Anthropic, &messages);

    assert!(openai > base);
    assert!(anthropic > openai);
}

#[test]
fn anthropic_provider_estimate_counts_structured_thinking_blocks() {
    let msg = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: Some(vec![
            AnthropicThinkingBlock {
                block_type: "thinking".into(),
                thinking: Some("hidden reasoning".into()),
                signature: Some("sig_123".into()),
                data: None,
            },
            AnthropicThinkingBlock {
                block_type: "redacted_thinking".into(),
                thinking: None,
                signature: None,
                data: Some("opaque_blob".into()),
            },
        ]),
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };

    let base = message_token_len(&msg);
    let openai = message_token_len_for_provider(Provider::OpenAI, &msg);
    let anthropic = message_token_len_for_provider(Provider::Anthropic, &msg);

    assert_eq!(openai, base);
    assert!(anthropic > openai);
}

#[test]
fn anthropic_provider_estimate_ignores_openai_responses_checkpoint_blocks() {
    let msg = ChatMessage {
        role: "assistant".into(),
        content: Some("done".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: Some(vec![AnthropicThinkingBlock {
            block_type: OPENAI_RESPONSES_RESPONSE_ID_BLOCK_TYPE.into(),
            thinking: None,
            signature: None,
            data: Some("resp_123".into()),
        }]),
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };

    let openai = message_token_len_for_provider(Provider::OpenAI, &msg);
    let anthropic = message_token_len_for_provider(Provider::Anthropic, &msg);

    assert_eq!(anthropic, openai);
}

#[test]
fn request_estimate_includes_tool_schema_overhead() {
    let messages = vec![ChatMessage {
        role: "system".into(),
        content: Some("system prompt".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let extra_tools = vec![json!({
        "name": "mcp__very_large_tool",
        "description": "A runtime MCP tool with a large schema payload.",
        "input_schema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "workspace path"},
                "content": {"type": "string", "description": "large content"},
                "flags": {
                    "type": "array",
                    "items": {"type": "string"}
                }
            },
            "required": ["path", "content"]
        }
    })];

    let message_estimate = estimate_tokens_for_provider(Provider::Anthropic, &messages);
    let request_estimate =
        estimate_request_tokens_for_provider(Provider::Anthropic, &messages, &extra_tools);

    assert!(request_estimate > message_estimate);
}

#[test]
fn openai_request_estimate_includes_builtin_tool_schemas() {
    let messages = vec![ChatMessage {
        role: "system".into(),
        content: Some("system prompt".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    let message_estimate = estimate_tokens_for_provider(Provider::OpenAI, &messages);
    let request_estimate = estimate_request_tokens_for_provider(Provider::OpenAI, &messages, &[]);

    assert!(request_estimate > message_estimate);
}

#[test]
fn context_input_budget_reserves_headroom() {
    let mut providers = HashMap::new();
    providers.insert(
        "anthropic".to_string(),
        JsonProviderConfig {
            base_url: "https://api.anthropic.com".to_string(),
            api_key: "anthropic-key".to_string(),
            api: "anthropic".to_string(),
            models: vec![JsonModelEntry {
                id: "claude-opus-4-7".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(1_000_000),
                max_tokens: Some(64_000),
                compat: None,
            }],
        },
    );

    let config = Config {
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
        providers,
        mcp_servers: HashMap::new(),
        port: 3000,
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
    };

    let budget = context_input_budget_for_model(&config, "anthropic/claude-opus-4-7");

    assert_eq!(budget, 900_000);
}

// ───── Phase 5: turn_len ─────

#[test]
fn turn_len_standalone_user() {
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hi".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    assert_eq!(turn_len(&messages, 0), 1);
}

#[test]
fn turn_len_user_plus_assistant() {
    let messages = vec![
        ChatMessage {
            role: "user".into(),
            content: Some("hi".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("hello".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    assert_eq!(turn_len(&messages, 0), 2);
}

#[test]
fn turn_len_user_assistant_with_tool_calls_and_results() {
    let messages = vec![
        ChatMessage {
            role: "user".into(),
            content: Some("list files".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "list_dir".into(),
                    arguments: "{}".into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("file1.txt\nfile2.txt".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
    ];
    // user + assistant(tool_calls) + 1 tool result = 3
    assert_eq!(turn_len(&messages, 0), 3);
}

#[test]
fn turn_len_orphan_assistant_with_tool_results() {
    let messages = vec![
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: "{}".into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("ok".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("ok2".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc2".into()),
            timestamp: None,
        },
    ];
    // assistant + 2 tool results = 3
    assert_eq!(turn_len(&messages, 0), 3);
}

#[test]
fn turn_len_standalone_assistant_text() {
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: Some("just text".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    assert_eq!(turn_len(&messages, 0), 1);
}

// ───── Phase 5: ChatMessage predicates ─────

#[test]
fn chat_message_has_nonempty_content() {
    let none_content = ChatMessage {
        role: "user".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!none_content.has_nonempty_content());

    let empty_content = ChatMessage {
        role: "user".into(),
        content: Some(String::new()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!empty_content.has_nonempty_content());

    let with_content = ChatMessage {
        role: "user".into(),
        content: Some("hello".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(with_content.has_nonempty_content());
}

#[test]
fn chat_message_has_tool_calls() {
    let none_tc = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!none_tc.has_tool_calls());

    let empty_tc = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![]),
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!empty_tc.has_tool_calls());

    let with_tc = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "exec".into(),
                arguments: "{}".into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    };
    assert!(with_tc.has_tool_calls());
}

#[test]
fn chat_message_with_thinking_is_not_empty_assistant_message() {
    let msg = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: Some("reasoning summary".into()),
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };

    assert!(!msg.is_empty_assistant_message());
}

#[test]
fn chat_message_is_empty_assistant_message() {
    let empty_asst = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(empty_asst.is_empty_assistant_message());

    let with_content = ChatMessage {
        role: "assistant".into(),
        content: Some("reply".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!with_content.is_empty_assistant_message());

    let with_thinking_blocks = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: Some(vec![AnthropicThinkingBlock {
            block_type: "thinking".into(),
            thinking: Some("reasoning".into()),
            signature: Some("sig_123".into()),
            data: None,
        }]),
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!with_thinking_blocks.is_empty_assistant_message());

    let user_msg = ChatMessage {
        role: "user".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!user_msg.is_empty_assistant_message());
}

// ───── Phase 5: prune_messages with tool_calls turn ─────

#[test]
fn prune_messages_removes_complete_tool_turn() {
    let big = "x".repeat(200_000);
    let mut messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("sys".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(big.clone()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: big.clone(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some(big.clone()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("latest".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    let before = messages.len();
    prune_messages(&mut messages, 1000);
    let pruned = before - messages.len();
    assert!(
        pruned >= 3,
        "should remove complete tool turn, pruned={pruned}"
    );
    assert_eq!(messages[0].role, "system");
    assert!(messages.last().unwrap().content.as_deref() == Some("latest"));
}

// ───── Phase 5: trim_incomplete_tool_calls no-op on complete transaction ─────

#[test]
fn trim_incomplete_tool_calls_preserves_complete_transaction() {
    let mut messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("sys".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("do something".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "tc1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "exec".into(),
                        arguments: r#"{"cmd":"ls"}"#.into(),
                    },
                },
                ToolCall {
                    id: "tc2".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: r#"{"path":"a.txt"}"#.into(),
                    },
                },
            ]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("result1".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("result2".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc2".into()),
            timestamp: None,
        },
    ];
    let before_len = messages.len();
    trim_incomplete_tool_calls(&mut messages);
    assert_eq!(messages.len(), before_len);
}

#[test]
fn trim_incomplete_tool_calls_removes_orphaned_assistant_and_partial_results() {
    let mut messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("sys".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("do something".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "tc1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "exec".into(),
                        arguments: "{}".into(),
                    },
                },
                ToolCall {
                    id: "tc2".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: None,
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                },
            ]),
            tool_call_id: None,
            timestamp: None,
        },
        // Only 1 of 2 tool results present —incomplete
        ChatMessage {
            role: "tool".into(),
            content: Some("result1".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
    ];
    trim_incomplete_tool_calls(&mut messages);
    // Should have removed the assistant + partial tool result, keeping system + user.
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, "system");
    assert_eq!(messages[1].role, "user");
}

// ───── Phase 5: tool_think ─────

#[test]
fn trim_incomplete_tool_calls_in_session_drops_orphaned_subagent_snapshots() {
    let mut session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: Some("sys".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "user".into(),
                content: Some("do something".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: None,
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: Some(vec![
                    ToolCall {
                        id: "tc1".into(),
                        call_type: "function".into(),
                        gemini_thought_signature: None,
                        function: FunctionCall {
                            name: "task".into(),
                            arguments: r#"{"agent":"reviewer","prompt":"one"}"#.into(),
                        },
                    },
                    ToolCall {
                        id: "tc2".into(),
                        call_type: "function".into(),
                        gemini_thought_signature: None,
                        function: FunctionCall {
                            name: "task".into(),
                            arguments: r#"{"agent":"reviewer","prompt":"two"}"#.into(),
                        },
                    },
                ]),
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: Some("partial result".into()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: Some("tc1".into()),
                timestamp: None,
            },
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 1,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: HashSet::from(["tc1".to_string()]),
        subagent_snapshots: HashMap::from([(
            subagent_snapshot_storage_key("tc1", 1),
            SubagentHistorySnapshot {
                result_excerpt: Some("partial result".into()),
                success: false,
                ..Default::default()
            },
        )]),
        todos: crate::todos::TodoSnapshot::default(),
        version: SESSION_VERSION,
        workspace: PathBuf::new(),
    };

    trim_incomplete_tool_calls_in_session(&mut session);

    assert_eq!(session.messages.len(), 2);
    assert!(session.subagent_snapshots.is_empty());
    assert!(session.failed_tool_results.is_empty());
}

#[test]
fn tool_think_records_thought() {
    let result = tools::exec::tool_think(&json!({"thought": "analyze the problem"}));
    assert!(result.contains("analyze the problem"));
    assert!(result.contains("Thought recorded:"));
}

#[test]
fn tool_think_fallback_when_no_thought() {
    let result = tools::exec::tool_think(&json!({}));
    assert!(result.contains("(no thought provided)"));
}

// ───── parse_serde_error_position ─────

#[test]
fn parse_serde_error_position_extracts_line_and_column() {
    let (line, col) =
        parse_serde_error_position("invalid type: map, expected a string at line 5 column 10");
    assert_eq!(line, Some(5));
    assert_eq!(col, Some(10));
}

#[test]
fn parse_serde_error_position_returns_none_for_no_match() {
    let (line, col) = parse_serde_error_position("something went wrong");
    assert_eq!(line, None);
    assert_eq!(col, None);
}

#[test]
fn save_session_replace_from_temp_restores_backup_on_failed_swap() {
    let base = std::env::temp_dir().join(format!("lingclaw-session-replace-{}", now_epoch()));
    let path = base.join("session.json");
    let tmp_path = base.join("session.json.tmp");
    let backup_path = base.join("session.json.lingclaw-save-backup");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(&path, "old-value").unwrap();

    let err = replace_session_file_from_temp(&path, &tmp_path)
        .expect_err("missing temp file should trigger rollback");

    assert!(!err.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old-value");
    assert!(!tmp_path.exists());
    assert!(!backup_path.exists());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn save_session_replace_from_temp_replaces_existing_file_with_stale_backup() {
    let base = std::env::temp_dir().join(format!(
        "lingclaw-session-replace-stale-backup-{}",
        now_epoch()
    ));
    let path = base.join("session.json");
    let tmp_path = base.join("session.json.tmp");
    let backup_path = base.join("session.json.lingclaw-save-backup");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(&path, "old-value").unwrap();
    std::fs::write(&tmp_path, "new-value").unwrap();
    std::fs::write(&backup_path, "stale-backup").unwrap();

    replace_session_file_from_temp(&path, &tmp_path).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new-value");
    assert!(!tmp_path.exists());
    assert!(!backup_path.exists());

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn replace_file_from_temp_replaces_existing_file_without_losing_data() {
    let base = std::env::temp_dir().join(format!("lingclaw-config-replace-{}", now_epoch()));
    let path = base.join(".lingclaw.json");
    let tmp_path = base.join(".lingclaw.json.tmp");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(&path, "old-value").unwrap();
    std::fs::write(&tmp_path, "new-value").unwrap();

    replace_file_from_temp(&path, &tmp_path).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "new-value");
    assert!(!tmp_path.exists());
    assert!(!base.join(".lingclaw.json.lingclaw-save-backup").exists());

    let _ = std::fs::remove_dir_all(&base);
}
