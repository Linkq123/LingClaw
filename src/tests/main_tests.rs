use super::*;
use crate::config::JsonMcpServerConfig;
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
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
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
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
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
        disabled_system_skills: HashSet::new(),
        failed_tool_results: HashSet::from(["task_1".to_string()]),
        subagent_snapshots: HashMap::new(),
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
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
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
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
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
    // An assistant message that has thinking but no content (e.g., think → tool_call
    // cycle with no text response) must appear in the history payload so that the
    // reasoning card is replayed after a page refresh.
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![
            ChatMessage {
                role: "assistant".into(),
                content: None, // no text — only thinking + tool_calls
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
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
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

#[test]
fn provider_detect_accepts_provider_prefixed_model_refs() {
    assert_eq!(
        Provider::detect(
            "anthropic/claude-sonnet-4-20250514",
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
                context_window: Some(200000),
                max_tokens: Some(8192),
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
                context_window: Some(200000),
                max_tokens: Some(8192),
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
                id: "claude-sonnet-4-20250514".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(200000),
                max_tokens: Some(8192),
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
    };

    let canonical = config
        .canonical_model_ref("claude-sonnet-4-20250514")
        .expect("unique model id should expand to provider/model");

    assert_eq!(canonical, "anthropic/claude-sonnet-4-20250514");
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
    };

    let resolved = config.resolve_model("anthropic/claude-sonnet-4-20250514");

    assert_eq!(resolved.provider, Provider::Anthropic);
    assert_eq!(resolved.api_base, "https://api.anthropic.com");
    assert_eq!(resolved.model_id, "claude-sonnet-4-20250514");
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
                id: "claude-sonnet-4-20250514".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(200000),
                max_tokens: Some(8192),
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
    };
    let mut session = test_session("abc", "Test", Some("anthropic/claude-sonnet-4-20250514"));
    session.think_level = "medium".to_string();

    let status = build_session_status(&session, &config);

    assert!(status.contains("model: anthropic/claude-sonnet-4-20250514"));
    assert!(status.contains("resolved_provider: anthropic"));
    assert!(status.contains("resolved_api_base: https://api.anthropic.com"));
    assert!(status.contains("resolved_model_id: claude-sonnet-4-20250514"));
    assert!(status.contains("max_tokens: 8.2K"));
    assert!(status.contains("context_est: 4/180K (limit 200K)"));
    assert!(status.contains("token_usage_source: input=estimated output=estimated"));
    assert!(status.contains("think: medium"));
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

    let usage = build_global_today_usage(&sessions);

    assert!(usage.contains("global_today_usage_est: # 所有会话今日 token 使用估算"));
    assert!(usage.contains("input_tokens: 3K"));
    assert!(usage.contains("output_tokens: 1K"));
    assert!(usage.contains("total_tokens: 4K"));
}

#[test]
fn gather_global_today_usage_uses_main_session_only_in_single_session_mode() {
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
    assert!(usage.contains("input_tokens: 2.3K"));
    assert!(usage.contains("output_tokens: 560"));
    assert!(usage.contains("total_tokens: 2.9K"));
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
    assert_eq!(summaries[0]["id"].as_str(), Some("broken-session"));
    assert_eq!(summaries[0]["corrupt"].as_bool(), Some(true));

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
    assert_eq!(summaries[0]["id"].as_str(), Some(session_id.as_str()));
    assert_eq!(summaries[0]["messages"].as_u64(), Some(0));
    assert_eq!(summaries[0]["corrupt"].as_bool(), Some(false));

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn resolve_or_create_socket_session_ignores_requested_session_and_uses_main() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let state = Arc::new(test_app_state());
    let (tx, mut rx) = mpsc::channel::<String>(4);

    let resolved = rt.block_on(resolve_or_create_socket_session(
        &state,
        &tx,
        Some("legacy-session"),
        1,
    ));

    assert_eq!(resolved, MAIN_SESSION_ID);
    assert!(
        rt.block_on(state.sessions.lock())
            .contains_key(MAIN_SESSION_ID)
    );

    let payload = rt
        .block_on(rx.recv())
        .expect("session payload should be sent");
    let parsed: serde_json::Value =
        serde_json::from_str(&payload).expect("payload should be valid json");
    assert_eq!(parsed["type"].as_str(), Some("session"));
    assert_eq!(parsed["id"].as_str(), Some(MAIN_SESSION_ID));
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
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
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
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
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
fn load_session_from_disk_trims_incomplete_tool_transaction() {
    let session_id = format!("trim-load-{}", now_epoch());
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

    let Json(payload) = api_usage(headers, State(state))
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

    let Json(payload) = api_usage(headers, State(state.clone()))
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
        disabled_system_skills: HashSet::new(),
        failed_tool_results: Default::default(),
        subagent_snapshots: HashMap::new(),
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
        },
        agent::ToolResultEntry {
            id: "c2".into(),
            name: "read_file".into(),
            result: "z\n".repeat(3000),
            duration_ms: 0,
            is_error: false,
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
fn auto_think_adapts_in_agent_loop_context() {
    // Simulate the pattern used in the Analyze arm:
    // auto mode + reasoning model —phase-adapted level
    let think_level = "auto";
    let model_supports_reasoning = true;

    // Cycle 0, no observation
    let effective = if think_level == "auto" && model_supports_reasoning {
        agent::auto_think_level(0, false, 0, 0).to_owned()
    } else {
        think_level.to_owned()
    };
    assert_eq!(effective, "medium");

    // Cycle 2, has observation
    let effective = if think_level == "auto" && model_supports_reasoning {
        agent::auto_think_level(2, true, 0, 0).to_owned()
    } else {
        think_level.to_owned()
    };
    assert_eq!(effective, "high");

    // Cycle 10, late round
    let effective = if think_level == "auto" && model_supports_reasoning {
        agent::auto_think_level(10, false, 0, 0).to_owned()
    } else {
        think_level.to_owned()
    };
    assert_eq!(effective, "low");

    // Explicit level —no adaptation
    let think_level = "high";
    let effective = if think_level == "auto" && model_supports_reasoning {
        agent::auto_think_level(5, true, 0, 0).to_owned()
    } else {
        think_level.to_owned()
    };
    assert_eq!(effective, "high");
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

    assert!(
        result
            .response
            .contains("LingClaw only keeps the main session")
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
        json!({
            "id": "empty-session",
            "messages": 0,
            "corrupt": false,
        }),
        json!({
            "id": "corrupt-session",
            "messages": 99,
            "corrupt": true,
        }),
        json!({
            "id": "real-session",
            "messages": 3,
            "corrupt": false,
        }),
    ];

    let recoverable = recoverable_session_ids_from_summaries(&summaries);

    assert_eq!(recoverable, vec!["real-session".to_string()]);
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
    let (live_tx, _live_rx) = mpsc::channel::<serde_json::Value>(4);
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
    let (live_tx, _live_rx) = mpsc::channel::<serde_json::Value>(4);
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
    let (live_tx, _live_rx) = mpsc::channel::<serde_json::Value>(4);
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

    for _ in 0..7 {
        let _ = rt
            .block_on(async { tokio::time::timeout(Duration::from_secs(2), bound_rx.recv()).await })
            .expect("bound replay event should arrive before timeout")
            .expect("bound client should receive live event");
    }

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed = (0..7)
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
    assert_eq!(replayed[1]["type"], "thinking_start");
    assert_eq!(replayed[2]["type"], "thinking_delta");
    assert_eq!(replayed[2]["content"], "step-1");
    assert_eq!(replayed[3]["type"], "thinking_done");
    assert_eq!(replayed[4]["type"], "tool_call");
    assert_eq!(replayed[4]["id"], "tool-1");
    assert_eq!(replayed[5]["type"], "tool_result");
    assert_eq!(replayed[5]["result"], "file contents");
    assert_eq!(replayed[6]["type"], "delta");
    assert_eq!(replayed[6]["content"], "final answer");

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
    assert_eq!(replayed[2]["type"], "task_tool");
    assert_eq!(replayed[2]["task_id"], "task-123");
    assert_eq!(replayed[3]["type"], "tool_result");
    assert_eq!(replayed[3]["task_id"], "task-123");
    assert_eq!(replayed[3]["subagent"], "coder");
    assert_eq!(replayed[3]["id"], "tool-a");
    assert_eq!(replayed[3]["duration_ms"], 42);
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
    ));
    assert!(outcome.is_error);

    // think tool is never an error
    let outcome = rt.block_on(tools::execute_tool(
        "think",
        r#"{"thought":"test"}"#,
        &test_config(),
        &reqwest::Client::new(),
        std::path::Path::new("."),
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
        },
        agent::ToolResultEntry {
            id: "err".into(),
            name: "exec".into(),
            result: "exec error: command not found".into(),
            duration_ms: 10,
            is_error: true,
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
    let s = "\u{4f60}\u{597d}\u{4e16}\u{754c}"; // 12 bytes (3 per char)
    let result = truncate(s, 7); // mid-char boundary
    // Should cut at char boundary <= 7, which is 6 (after first 2 chars)
    assert!(result.starts_with("\u{4f60}\u{597d}"));
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
    let mut s = "\u{4f60}\u{597d}\u{4e16}".to_string(); // 9 bytes
    truncate_safe(&mut s, 7);
    assert_eq!(s, "\u{4f60}\u{597d}");

    // Emoji: 4 bytes per char. Cutting at 5 must round down to 4.
    let mut s = "\u{1F980}\u{1F980}".to_string(); // 8 bytes
    truncate_safe(&mut s, 5);
    assert_eq!(s, "\u{1F980}");

    // Already within limit —unchanged.
    let mut s = "hello".to_string();
    truncate_safe(&mut s, 100);
    assert_eq!(s, "hello");
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
                id: "claude-sonnet-4-20250514".to_string(),
                name: None,
                reasoning: Some(false),
                input: None,
                cost: None,
                context_window: Some(200000),
                max_tokens: Some(8192),
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
    };

    let budget = context_input_budget_for_model(&config, "anthropic/claude-sonnet-4-20250514");

    assert_eq!(budget, 180_000);
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
