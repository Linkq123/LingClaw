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
        memory_model: None,

        reflection_model: None,
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
        upload_token: "test-upload-token".to_string(),
        hooks: HookRegistry::new(),
        memory_queue: None,
    }
}

fn test_app_state_with_config(config: Config) -> AppState {
    AppState {
        config,
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
        upload_token: "test-upload-token".to_string(),
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
            images: None,
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

fn make_message(role: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: Some(content.to_string()),
        images: None,
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
        memory_model: None,

        reflection_model: None,
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
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: Some(long_raw_result.clone()),
                images: None,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        disabled_system_skills: HashSet::new(),
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
        model_override: None,
        think_level: default_think_level(),
        show_react: default_show_react(),
        show_tools: default_show_tools(),
        show_reasoning: default_show_reasoning(),
        disabled_system_skills: HashSet::new(),
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
        memory_model: None,

        reflection_model: None,
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("a".repeat(500)),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("b".repeat(500)),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("keep".into()),
            images: None,
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(1),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some(String::new()),
            images: None,
            tool_calls: Some(vec![ToolCall {
                id: "call-1".into(),
                call_type: "function".into(),
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
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: None,
                images: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(1773669433),
            },
            ChatMessage {
                role: "user".into(),
                content: Some("next".into()),
                images: None,
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
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
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
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
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
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: Some(String::new()),
                images: None,
                tool_calls: Some(vec![ToolCall {
                    id: "call_obs".into(),
                    call_type: "function".into(),
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
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        disabled_system_skills: HashSet::new(),
        version: 0,
        workspace: PathBuf::new(),
    };

    let payload = build_history_payload(&session);
    let msgs = payload["messages"].as_array().unwrap();
    let tool_entry = msgs.iter().find(|m| m["role"] == "tool_result").unwrap();
    let result_str = tool_entry["result"].as_str().unwrap();

    // Must be exact raw content — no "[Observation:" prefix
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
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };

    let summaries = vec![agent::ObservationSummary {
        tool_call_id: "c1".into(),
        tool_name: "exec".into(),
        byte_size: 8000,
        line_count: 200,
        hint: "exec returned 200 lines / 8000 bytes — focus on key findings".into(),
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
    // auto mode + reasoning model — phase-adapted level
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

    // Explicit level — no adaptation
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
            .block_on(bound_rx.recv())
            .expect("bound client should receive live event");
    }

    let (replay_tx, mut replay_rx) = mpsc::channel::<String>(16);
    rt.block_on(replay_live_round(&replay_tx, &state, &session_id));

    let replayed = (0..7)
        .map(|_| {
            let raw = rt
                .block_on(replay_rx.recv())
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

    // Unknown tool — is_error
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("a".repeat(200_000)),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("b".repeat(200_000)),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("latest".into()),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    let before = messages.len();
    prune_messages(&mut messages, 1000); // very small limit — must prune
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
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    // (0 + 0 + 10) / 4 = 2
    assert_eq!(message_token_len(&msg), 2);
}

#[test]
fn message_token_len_content_only() {
    let msg = ChatMessage {
        role: "user".into(),
        content: Some("hello world".into()), // 11 chars
        images: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    // (11 + 0 + 10) / 4 = 5
    assert_eq!(message_token_len(&msg), 5);
}

#[test]
fn message_token_len_with_tool_calls() {
    let msg = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "exec".into(),                 // 4
                arguments: r#"{"cmd":"ls"}"#.into(), // 12
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    };
    // (0 + (4+12) + 10) / 4 = 26/4 = 6
    assert_eq!(message_token_len(&msg), 6);
}

#[test]
fn estimate_tokens_sums_messages() {
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("sys".into()), // 3
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()), // 5
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    // (3+0+10)/4 + (5+0+10)/4 = 3 + 3 = 6
    assert_eq!(estimate_tokens(&messages), 6);
}

#[test]
fn provider_aware_estimate_adds_tool_protocol_overhead() {
    let messages = vec![
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
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
fn request_estimate_includes_tool_schema_overhead() {
    let messages = vec![ChatMessage {
        role: "system".into(),
        content: Some("system prompt".into()),
        images: None,
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
        memory_model: None,

        reflection_model: None,
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("hello".into()),
            images: None,
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
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
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
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
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("ok2".into()),
            images: None,
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
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!none_content.has_nonempty_content());

    let empty_content = ChatMessage {
        role: "user".into(),
        content: Some(String::new()),
        images: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!empty_content.has_nonempty_content());

    let with_content = ChatMessage {
        role: "user".into(),
        content: Some("hello".into()),
        images: None,
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
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!none_tc.has_tool_calls());

    let empty_tc = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        tool_calls: Some(vec![]),
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!empty_tc.has_tool_calls());

    let with_tc = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
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
fn chat_message_is_empty_assistant_message() {
    let empty_asst = ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(empty_asst.is_empty_assistant_message());

    let with_content = ChatMessage {
        role: "assistant".into(),
        content: Some("reply".into()),
        images: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!with_content.is_empty_assistant_message());

    let user_msg = ChatMessage {
        role: "user".into(),
        content: None,
        images: None,
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(big.clone()),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
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
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("latest".into()),
            images: None,
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("do something".into()),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "tc1".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "exec".into(),
                        arguments: r#"{"cmd":"ls"}"#.into(),
                    },
                },
                ToolCall {
                    id: "tc2".into(),
                    call_type: "function".into(),
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
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("result2".into()),
            images: None,
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("do something".into()),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "tc1".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "exec".into(),
                        arguments: "{}".into(),
                    },
                },
                ToolCall {
                    id: "tc2".into(),
                    call_type: "function".into(),
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                },
            ]),
            tool_call_id: None,
            timestamp: None,
        },
        // Only 1 of 2 tool results present — incomplete
        ChatMessage {
            role: "tool".into(),
            content: Some("result1".into()),
            images: None,
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
