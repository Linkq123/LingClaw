use super::*;

fn test_config() -> Config {
    Config {
        api_key: "env-key".to_string(),
        api_base: "https://fallback.example/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        provider: Provider::OpenAI,
        providers: HashMap::new(),
        port: 3000,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
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
fn resolve_session_target_accepts_unique_prefix() {
    let known_ids = HashSet::from([
        "main".to_string(),
        "abc1234567890".to_string(),
        "def9999999999".to_string(),
    ]);

    let resolved =
        resolve_session_target("abc123", &known_ids).expect("prefix should resolve");

    assert_eq!(resolved, "abc1234567890");
}

#[test]
fn resolve_session_target_rejects_ambiguous_prefix() {
    let known_ids = HashSet::from([
        "abc1234567890".to_string(),
        "abc1239999999".to_string(),
    ]);

    let err =
        resolve_session_target("abc123", &known_ids).expect_err("prefix should be ambiguous");

    assert!(err.contains("ambiguous"));
}

#[test]
fn list_saved_session_ids_in_dir_uses_filenames_even_for_invalid_json() {
    let base = std::env::temp_dir().join(format!("lingclaw-test-{}", now_epoch()));
    std::fs::create_dir_all(&base).expect("temp dir should be created");
    std::fs::write(base.join("good-session.json"), "not valid json")
        .expect("invalid json file should be created");
    std::fs::write(base.join("ignored.txt"), "ignore me")
        .expect("non-json file should be created");

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