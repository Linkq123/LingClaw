use super::*;
use serde_json::json;
use std::sync::atomic::AtomicU64;

fn test_config() -> Config {
    Config {
        api_key: "env-key".to_string(),
        api_base: "https://fallback.example/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        provider: Provider::OpenAI,
        providers: HashMap::new(),
        port: DEFAULT_PORT,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
    }
}

#[test]
fn default_port_constant_is_18989() {
    assert_eq!(DEFAULT_PORT, 18989);
}

fn test_app_state() -> AppState {
    AppState {
        config: test_config(),
        http: reqwest::Client::new(),
        sessions: Mutex::new(HashMap::new()),
        active_connections: Mutex::new(HashMap::new()),
        session_clients: Mutex::new(HashMap::new()),
        live_rounds: Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
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
        model_override: model_override.map(|value| value.to_string()),
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        version: 0,
        workspace: PathBuf::new(),
        avatar: None,
    }
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
        provider: Provider::OpenAI,
        providers,
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
    };

    let resolved = config.resolve_model("gpt-4o-mini");

    assert_eq!(resolved.model_id, "gpt-4o-mini");
    assert_eq!(resolved.api_base, "https://api.openai.com/v1");
    assert_eq!(resolved.api_key, "test-key");
    assert_eq!(resolved.max_tokens, Some(16384));
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
fn build_history_payload_preserves_raw_tool_result_content() {
    let long_raw_result = format!("{{\"ok\":true,\"payload\":\"{}\"}}", "x".repeat(5000));
    let session = Session {
        id: "test".into(),
        name: "Test".into(),
        messages: vec![
            ChatMessage {
                role: "system".into(),
                content: Some("system".into()),
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "tool".into(),
                content: Some(long_raw_result.clone()),
                tool_calls: None,
                tool_call_id: Some("call_1".into()),
                timestamp: Some(123),
            },
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 1,
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        version: 0,
        workspace: PathBuf::new(),
        avatar: None,
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
        provider: Provider::OpenAI,
        providers,
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
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
        provider: Provider::OpenAI,
        providers,
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
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
        provider: Provider::OpenAI,
        providers,
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
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
        provider: Provider::OpenAI,
        providers,
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
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
        provider: Provider::OpenAI,
        providers,
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
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
        provider: Provider::OpenAI,
        providers,
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
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
        provider: Provider::OpenAI,
        providers,
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
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
        provider: Provider::OpenAI,
        providers,
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
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
        provider: Provider::OpenAI,
        providers: HashMap::new(),
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
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
        provider: Provider::OpenAI,
        providers: HashMap::new(),
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
    };

    let resolved = config.resolve_model("anthropic/claude-sonnet-4-20250514");

    assert_eq!(resolved.provider, Provider::Anthropic);
    assert_eq!(resolved.api_base, "https://api.anthropic.com");
    assert_eq!(resolved.model_id, "claude-sonnet-4-20250514");
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
        provider: Provider::OpenAI,
        providers,
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
    };
    let mut session = test_session("abc", "Test", Some("anthropic/claude-sonnet-4-20250514"));
    session.think_level = "medium".to_string();

    let status = build_session_status(&session, &config);

    assert!(status.contains("model: anthropic/claude-sonnet-4-20250514"));
    assert!(status.contains("resolved_provider: anthropic"));
    assert!(status.contains("resolved_api_base: https://api.anthropic.com"));
    assert!(status.contains("resolved_model_id: claude-sonnet-4-20250514"));
    assert!(status.contains("max_tokens: 8192"));
    assert!(status.contains("think: medium"));
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
fn resolved_main_prefix_is_still_protected() {
    let known_ids = HashSet::from([MAIN_SESSION_ID.to_string(), "misc-session".to_string()]);

    let resolved = resolve_session_target("ma", &known_ids).expect("prefix should resolve");

    assert_eq!(resolved, MAIN_SESSION_ID);
}

#[test]
fn build_active_session_lines_lists_only_active_sessions_with_full_ids() {
    let config = test_config();
    let sessions = HashMap::from([
        (
            MAIN_SESSION_ID.to_string(),
            test_session(MAIN_SESSION_ID, "Main", None),
        ),
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
    assert!(!lines[0].contains("Idle"));
}

#[test]
fn prune_messages_removes_complete_turns_without_recomputing_from_scratch() {
    let mut messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("system".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("a".repeat(500)),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("b".repeat(500)),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("keep".into()),
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(1),
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some(String::new()),
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
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(1773669433),
            },
            ChatMessage {
                role: "user".into(),
                content: Some("next".into()),
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
        ],
        created_at: 1,
        updated_at: 1,
        tool_calls_count: 0,
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        version: 0,
        workspace: workspace.clone(),
        avatar: None,
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        }],
        created_at: 1,
        updated_at: 1,
        tool_calls_count: 0,
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        version: 1,
        workspace: workspace.clone(),
        avatar: None,
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
    let token = generate_shutdown_token();

    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|ch| ch.is_ascii_hexdigit()));
}

#[test]
fn parse_identity_avatar_treats_none_as_unset() {
    let base = std::env::temp_dir().join(format!("lingclaw-avatar-none-{}", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");
    std::fs::write(base.join("IDENTITY.md"), "- 头像：none\n")
        .expect("identity file should be written");

    let avatar = prompts::parse_identity_avatar(&base);

    assert_eq!(avatar, None);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn parse_identity_avatar_ignores_legacy_unset_placeholder_value() {
    let base = std::env::temp_dir().join(format!("lingclaw-avatar-placeholder-{}", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");
    std::fs::write(base.join("IDENTITY.md"), "- 头像：暂未设置\n")
        .expect("identity file should be written");

    let avatar = prompts::parse_identity_avatar(&base);

    assert_eq!(avatar, None);

    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn parse_identity_avatar_keeps_real_text_avatar() {
    let base = std::env::temp_dir().join(format!("lingclaw-avatar-text-{}", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");
    std::fs::write(base.join("IDENTITY.md"), "- 头像：✨\n")
        .expect("identity file should be written");

    let avatar = prompts::parse_identity_avatar(&base);

    assert_eq!(avatar.as_deref(), Some("✨"));

    let _ = std::fs::remove_dir_all(&base);
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
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
            ChatMessage {
                role: "assistant".into(),
                content: Some(String::new()),
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
                tool_calls: None,
                tool_call_id: Some("call_obs".into()),
                timestamp: Some(101),
            },
        ],
        created_at: 0,
        updated_at: 0,
        tool_calls_count: 1,
        model_override: None,
        think_level: default_think_level(),
        show_react: false,
        show_tools: true,
        show_reasoning: true,
        version: 0,
        workspace: PathBuf::new(),
        avatar: None,
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

    let hint = agent::build_observation_context_hint(&summaries);
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
    if let Some(hint) = agent::build_observation_context_hint(&summaries) {
        if let Some(ref mut content) = msg.content {
            content.push_str("\n\n");
            content.push_str(&hint);
        }
    }

    let content = msg.content.as_deref().unwrap();
    assert!(content.starts_with("You are an assistant."));
    assert!(content.contains("## Recent Observation Notes"));
    assert!(content.contains("**exec**"));
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
fn finish_reason_label_appears_in_done_event_shape() {
    // Verify FinishReason labels are valid strings for the done event
    assert_eq!(agent::FinishReason::Complete.label(), "complete");
    assert_eq!(agent::FinishReason::Empty.label(), "empty");
}

#[test]
fn auto_think_adapts_in_agent_loop_context() {
    // Simulate the pattern used in the Analyze arm:
    // auto mode + reasoning model → phase-adapted level
    let think_level = "auto";
    let model_supports_reasoning = true;

    // Cycle 0, no observation
    let effective = if think_level == "auto" && model_supports_reasoning {
        agent::auto_think_level(0, false).to_owned()
    } else {
        think_level.to_owned()
    };
    assert_eq!(effective, "medium");

    // Cycle 2, has observation
    let effective = if think_level == "auto" && model_supports_reasoning {
        agent::auto_think_level(2, true).to_owned()
    } else {
        think_level.to_owned()
    };
    assert_eq!(effective, "high");

    // Cycle 10, late round
    let effective = if think_level == "auto" && model_supports_reasoning {
        agent::auto_think_level(10, false).to_owned()
    } else {
        think_level.to_owned()
    };
    assert_eq!(effective, "low");

    // Explicit level → no adaptation
    let think_level = "high";
    let effective = if think_level == "auto" && model_supports_reasoning {
        agent::auto_think_level(5, true).to_owned()
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
fn claim_requested_session_waits_for_active_connection_release() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let session_id = format!("reclaim-refresh-{}", now_epoch());
    let state = Arc::new(test_app_state());
    let old_connection_id = 1;
    let new_connection_id = 2;

    {
        let mut sessions = rt.block_on(state.sessions.lock());
        sessions.insert(
            session_id.clone(),
            test_session(&session_id, "Reconnect", None),
        );
    }
    {
        let mut active = rt.block_on(state.active_connections.lock());
        active.insert(session_id.clone(), old_connection_id);
    }

    let release_state = state.clone();
    let release_id = session_id.clone();
    rt.spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        release_state
            .active_connections
            .lock()
            .await
            .remove(&release_id);
    });

    let claimed = rt.block_on(claim_requested_session(
        &session_id,
        &state,
        new_connection_id,
    ));
    assert_eq!(claimed.as_deref(), Some(session_id.as_str()));

    let _ = rt
        .block_on(state.active_connections.lock())
        .remove(&session_id);
    let _ = rt.block_on(state.sessions.lock()).remove(&session_id);
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
        json!({
            "type": "start",
            "round": 3,
            "avatar": "avatar-data",
            "phase": "act",
            "cycle": 2,
            "react_visible": true,
        }),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        json!({"type": "thinking_start"}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        json!({"type": "thinking_delta", "content": "step-1"}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
        json!({"type": "thinking_done"}),
    ));
    rt.block_on(dispatch_live_event(
        &state,
        &session_id,
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
        json!({"type": "done"}),
    ));
    assert!(rt
        .block_on(state.live_rounds.lock())
        .get(&session_id)
        .is_none());
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

    // Unknown tool → is_error
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("a".repeat(200_000)),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("b".repeat(200_000)),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("latest".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    let before = messages.len();
    prune_messages(&mut messages, 1000); // very small limit → must prune
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()), // 5
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    // (3+0+10)/4 + (5+0+10)/4 = 3 + 3 = 6
    assert_eq!(estimate_tokens(&messages), 6);
}

// ───── Phase 5: turn_len ─────

#[test]
fn turn_len_standalone_user() {
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hi".into()),
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("hello".into()),
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
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
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("ok2".into()),
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
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!none_content.has_nonempty_content());

    let empty_content = ChatMessage {
        role: "user".into(),
        content: Some(String::new()),
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!empty_content.has_nonempty_content());

    let with_content = ChatMessage {
        role: "user".into(),
        content: Some("hello".into()),
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
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!none_tc.has_tool_calls());

    let empty_tc = ChatMessage {
        role: "assistant".into(),
        content: None,
        tool_calls: Some(vec![]),
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!empty_tc.has_tool_calls());

    let with_tc = ChatMessage {
        role: "assistant".into(),
        content: None,
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
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(empty_asst.is_empty_assistant_message());

    let with_content = ChatMessage {
        role: "assistant".into(),
        content: Some("reply".into()),
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!with_content.is_empty_assistant_message());

    let user_msg = ChatMessage {
        role: "user".into(),
        content: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    };
    assert!(!user_msg.is_empty_assistant_message());
}

// ───── Phase 5: is_admin_tool ─────

#[test]
fn is_admin_tool_recognizes_admin_tools() {
    assert!(is_admin_tool("list_sessions"));
    assert!(is_admin_tool("delete_session"));
}

#[test]
fn is_admin_tool_rejects_regular_tools() {
    assert!(!is_admin_tool("exec"));
    assert!(!is_admin_tool("read_file"));
    assert!(!is_admin_tool("think"));
    assert!(!is_admin_tool("http_fetch"));
}

// ───── Phase 5: prune_messages with tool_calls turn ─────

#[test]
fn prune_messages_removes_complete_tool_turn() {
    let big = "x".repeat(200_000);
    let mut messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("sys".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(big.clone()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
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
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("latest".into()),
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("do something".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
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
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("result2".into()),
            tool_calls: None,
            tool_call_id: Some("tc2".into()),
            timestamp: None,
        },
    ];
    let before_len = messages.len();
    trim_incomplete_tool_calls(&mut messages);
    assert_eq!(messages.len(), before_len);
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
