use super::*;
use std::{collections::HashMap, time::Duration};

fn runtime_alignment_config(
    provider: Provider,
    api_base: &str,
    api_key: &str,
    model: &str,
    providers: HashMap<String, JsonProviderConfig>,
) -> Config {
    Config {
        api_key: api_key.to_string(),
        api_base: api_base.to_string(),
        model: model.to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,
        reflection_model: None,
        context_model: None,
        provider,
        openai_stream_include_usage: false,
        anthropic_prompt_caching: false,
        providers,
        mcp_servers: HashMap::new(),
        port: DEFAULT_PORT,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        tool_timeout: Duration::from_secs(30),
        sub_agent_timeout: Duration::from_secs(300),
        max_llm_retries: 2,
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
        structured_memory: false,
        daily_reflection: false,
        s3: None,
    }
}

#[test]
fn align_runtime_provider_config_uses_primary_provider_entry() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "sk-openai-primary".to_string(),
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
    providers.insert(
        "openai-2".to_string(),
        JsonProviderConfig {
            base_url: "https://openai-gateway.example/v1".to_string(),
            api_key: "sk-openai-secondary".to_string(),
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

    let mut config = runtime_alignment_config(
        Provider::OpenAI,
        Provider::OpenAI.default_api_base(),
        "env-openai-key",
        "openai-2/gpt-4o-mini",
        providers,
    );

    align_runtime_provider_config(&mut config, true, true, true);

    assert_eq!(config.provider, Provider::OpenAI);
    assert_eq!(config.api_base, "https://openai-gateway.example/v1");
    assert_eq!(config.api_key, "sk-openai-secondary");
}

#[test]
fn align_runtime_provider_config_updates_provider_family_from_primary_model() {
    let mut providers = HashMap::new();
    providers.insert(
        "anthropic-2".to_string(),
        JsonProviderConfig {
            base_url: "https://anthropic-gateway.example".to_string(),
            api_key: "sk-ant-secondary".to_string(),
            api: "anthropic".to_string(),
            models: vec![JsonModelEntry {
                id: "claude-haiku-3-20250306".to_string(),
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

    let mut config = runtime_alignment_config(
        Provider::OpenAI,
        Provider::OpenAI.default_api_base(),
        "env-openai-key",
        "anthropic-2/claude-haiku-3-20250306",
        providers,
    );

    align_runtime_provider_config(&mut config, true, true, true);

    assert_eq!(config.provider, Provider::Anthropic);
    assert_eq!(config.api_base, "https://anthropic-gateway.example");
    assert_eq!(config.api_key, "sk-ant-secondary");
}

#[test]
fn resolve_provider_name_prefers_configured_provider_alias() {
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

    let config = runtime_alignment_config(
        Provider::OpenAI,
        "https://api-b.example/v1",
        "key-b",
        "shared-model",
        providers,
    );

    assert_eq!(config.resolve_provider_name("shared-model"), "openai-b");
}

#[test]
fn model_supports_image_prefers_runtime_aligned_provider_for_plain_ids() {
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
                input: Some(vec!["text".to_string()]),
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
                input: Some(vec!["text".to_string(), "image".to_string()]),
                cost: None,
                context_window: Some(128000),
                max_tokens: Some(8192),
                compat: None,
            }],
        },
    );

    let config = runtime_alignment_config(
        Provider::OpenAI,
        "https://api-b.example/v1",
        "key-b",
        "shared-model",
        providers,
    );

    assert_eq!(
        config.resolved_model_ref("shared-model"),
        "openai-b/shared-model"
    );
    assert!(config.model_supports_image("shared-model"));
}

#[test]
fn gemini_is_supported_as_builtin_provider() {
    assert_eq!(Provider::from_api_kind("gemini"), Provider::Gemini);
    assert_eq!(
        Provider::Gemini.default_api_base(),
        "https://generativelanguage.googleapis.com/v1beta"
    );
    assert_eq!(Provider::Gemini.api_key_env_var(), Some("GEMINI_API_KEY"));
    assert_eq!(
        Provider::Gemini.api_key_env_hint(),
        Some("GEMINI_API_KEY or GOOGLE_API_KEY")
    );
    assert_eq!(
        Provider::detect("gemini-2.5-flash", "", None),
        Provider::Gemini
    );
    assert!(is_builtin_provider_name("gemini"));
    assert!(validate_provider_api_kind("gemini").is_ok());
}

#[test]
fn empty_provider_config_accepts_gemini_model_prefix() {
    let config = runtime_alignment_config(
        Provider::Gemini,
        Provider::Gemini.default_api_base(),
        "gemini-key",
        "gemini/gemini-2.5-flash",
        HashMap::new(),
    );

    assert_eq!(
        config
            .canonical_model_ref("gemini/gemini-2.5-flash")
            .unwrap(),
        "gemini/gemini-2.5-flash"
    );
    assert_eq!(
        config.resolve_model("gemini/gemini-2.5-flash").provider,
        Provider::Gemini
    );
    assert_eq!(config.resolve_provider_name("gemini-2.5-flash"), "gemini");
}

#[test]
fn validate_json_provider_names_rejects_invalid_provider_keys() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai/test".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "key".to_string(),
            api: "openai-completions".to_string(),
            models: Vec::new(),
        },
    );
    let json_cfg = JsonConfig {
        settings: None,
        models: Some(JsonModelsConfig {
            providers: Some(providers),
        }),
        agents: None,
        mcp_servers: None,
        s3: None,
    };

    let err = validate_json_provider_names(&json_cfg)
        .expect_err("invalid provider keys should be rejected");
    assert!(err.contains("openai/test"));
    assert!(err.contains("cannot contain '/'"));
}

#[test]
fn validate_json_agent_model_refs_rejects_unknown_provider_alias() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai-work".to_string(),
        JsonProviderConfig {
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "key".to_string(),
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
    let json_cfg = JsonConfig {
        settings: None,
        models: Some(JsonModelsConfig {
            providers: Some(providers),
        }),
        agents: Some(JsonAgentsConfig {
            defaults: Some(JsonAgentDefaults {
                model: Some(JsonDefaultModel {
                    primary: Some("missing/gpt-4o-mini".to_string()),
                    fast: None,
                    sub_agent: None,
                    memory: None,
                    reflection: None,
                    context: None,
                    extra: HashMap::new(),
                }),
            }),
        }),
        mcp_servers: None,
        s3: None,
    };

    let err = validate_json_agent_model_refs(&json_cfg)
        .expect_err("unknown provider aliases should be rejected");
    assert!(err.contains("agents.defaults.model.primary"));
    assert!(err.contains("missing"));
}

#[test]
fn validate_json_agent_model_refs_rejects_unknown_prefix_without_provider_config() {
    let json_cfg = JsonConfig {
        settings: None,
        models: None,
        agents: Some(JsonAgentsConfig {
            defaults: Some(JsonAgentDefaults {
                model: Some(JsonDefaultModel {
                    primary: Some("missing/gpt-4o-mini".to_string()),
                    fast: None,
                    sub_agent: None,
                    memory: None,
                    reflection: None,
                    context: None,
                    extra: HashMap::new(),
                }),
            }),
        }),
        mcp_servers: None,
        s3: None,
    };

    let err = validate_json_agent_model_refs(&json_cfg)
        .expect_err("unknown provider prefixes should be rejected without provider config");
    assert!(err.contains("agents.defaults.model.primary"));
    assert!(err.contains("missing"));
}

#[test]
fn validate_json_agent_model_refs_rejects_unknown_model_for_configured_provider() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai-work".to_string(),
        JsonProviderConfig {
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "key".to_string(),
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
    let json_cfg = JsonConfig {
        settings: None,
        models: Some(JsonModelsConfig {
            providers: Some(providers),
        }),
        agents: Some(JsonAgentsConfig {
            defaults: Some(JsonAgentDefaults {
                model: Some(JsonDefaultModel {
                    primary: Some("openai-work/typo-model".to_string()),
                    fast: None,
                    sub_agent: None,
                    memory: None,
                    reflection: None,
                    context: None,
                    extra: HashMap::new(),
                }),
            }),
        }),
        mcp_servers: None,
        s3: None,
    };

    let err = validate_json_agent_model_refs(&json_cfg)
        .expect_err("unknown configured model ids should be rejected");
    assert!(err.contains("unknown model 'typo-model'"));
    assert!(err.contains("openai-work"));
}

#[test]
fn validate_json_agent_model_refs_checks_dynamic_sub_agent_overrides() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai-work".to_string(),
        JsonProviderConfig {
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "key".to_string(),
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
    let mut extra = HashMap::new();
    extra.insert(
        "sub-agent-reviewer".to_string(),
        serde_json::Value::String("missing/gpt-4o-mini".to_string()),
    );
    let json_cfg = JsonConfig {
        settings: None,
        models: Some(JsonModelsConfig {
            providers: Some(providers),
        }),
        agents: Some(JsonAgentsConfig {
            defaults: Some(JsonAgentDefaults {
                model: Some(JsonDefaultModel {
                    primary: Some("openai-work/gpt-4o-mini".to_string()),
                    fast: None,
                    sub_agent: None,
                    memory: None,
                    reflection: None,
                    context: None,
                    extra,
                }),
            }),
        }),
        mcp_servers: None,
        s3: None,
    };

    let err = validate_json_agent_model_refs(&json_cfg)
        .expect_err("dynamic sub-agent model overrides should be validated");
    assert!(err.contains("agents.defaults.model.sub-agent-reviewer"));
    assert!(err.contains("missing"));
}

#[test]
fn json_default_model_deserializes_null_override_and_unknown_nested_fields() {
    let json = r#"
    {
        "agents": {
            "defaults": {
                "model": {
                    "primary": "openai/gpt-4o-mini",
                    "sub-agent-reviewer": null,
                    "future-extension": {
                        "enabled": true
                    }
                }
            }
        }
    }
    "#;

    let parsed: JsonConfig =
        serde_json::from_str(json).expect("null override and nested extras should deserialize");
    let extra = &parsed
        .agents
        .as_ref()
        .and_then(|agents| agents.defaults.as_ref())
        .and_then(|defaults| defaults.model.as_ref())
        .expect("model defaults should parse")
        .extra;

    assert!(matches!(
        extra.get("sub-agent-reviewer"),
        Some(serde_json::Value::Null)
    ));
    assert_eq!(
        extra
            .get("future-extension")
            .and_then(|value| value.get("enabled"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

#[test]
fn validate_json_agent_model_refs_allows_null_dynamic_sub_agent_override() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai-work".to_string(),
        JsonProviderConfig {
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "key".to_string(),
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
    let mut extra = HashMap::new();
    extra.insert("sub-agent-reviewer".to_string(), serde_json::Value::Null);
    let json_cfg = JsonConfig {
        settings: None,
        models: Some(JsonModelsConfig {
            providers: Some(providers),
        }),
        agents: Some(JsonAgentsConfig {
            defaults: Some(JsonAgentDefaults {
                model: Some(JsonDefaultModel {
                    primary: Some("openai-work/gpt-4o-mini".to_string()),
                    fast: None,
                    sub_agent: None,
                    memory: None,
                    reflection: None,
                    context: None,
                    extra,
                }),
            }),
        }),
        mcp_servers: None,
        s3: None,
    };

    validate_json_agent_model_refs(&json_cfg)
        .expect("null dynamic sub-agent overrides should behave like an unset value");
}

#[test]
fn sanitize_loaded_json_config_filters_invalid_provider_names() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai/test".to_string(),
        JsonProviderConfig {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: "key".to_string(),
            api: "openai-completions".to_string(),
            models: Vec::new(),
        },
    );

    let sanitized = sanitize_loaded_json_config(JsonConfig {
        settings: Some(JsonSettings {
            port: Some(DEFAULT_PORT),
            ..Default::default()
        }),
        models: Some(JsonModelsConfig {
            providers: Some(providers),
        }),
        agents: None,
        mcp_servers: None,
        s3: None,
    });

    assert!(
        sanitized
            .models
            .as_ref()
            .and_then(|models| models.providers.as_ref())
            .is_none()
    );
    assert!(sanitized.agents.is_none());
    assert_eq!(
        sanitized.settings.and_then(|settings| settings.port),
        Some(DEFAULT_PORT)
    );
}

#[test]
fn validate_json_mcp_servers_rejects_empty_command() {
    let mut mcp_servers = HashMap::new();
    mcp_servers.insert(
        "empty-command".to_string(),
        JsonMcpServerConfig {
            command: "".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            enabled: true,
            timeout_secs: None,
        },
    );

    let err = Config::validate_json_mcp_servers(&JsonConfig {
        settings: None,
        models: None,
        agents: None,
        mcp_servers: Some(mcp_servers),
        s3: None,
    })
    .expect_err("empty MCP command should be rejected");

    assert!(err.contains("mcpServers.empty-command"));
}

#[test]
fn validate_json_provider_names_rejects_invalid_api_kind() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai-work".to_string(),
        JsonProviderConfig {
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "key".to_string(),
            api: "anthorpic".to_string(),
            models: Vec::new(),
        },
    );

    let err = validate_json_provider_names(&JsonConfig {
        settings: None,
        models: Some(JsonModelsConfig {
            providers: Some(providers),
        }),
        agents: None,
        mcp_servers: None,
        s3: None,
    })
    .expect_err("unsupported provider api kinds should be rejected");

    assert!(err.contains("openai-work"));
    assert!(err.contains("unsupported api 'anthorpic'"));
}

#[test]
fn validate_json_provider_names_allows_case_insensitive_api_kind() {
    let mut providers = HashMap::new();
    providers.insert(
        "anthropic-work".to_string(),
        JsonProviderConfig {
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "key".to_string(),
            api: "Anthropic".to_string(),
            models: Vec::new(),
        },
    );

    validate_json_provider_names(&JsonConfig {
        settings: None,
        models: Some(JsonModelsConfig {
            providers: Some(providers),
        }),
        agents: None,
        mcp_servers: None,
        s3: None,
    })
    .expect("case-insensitive provider api kinds should be accepted");
}

#[test]
fn validate_json_provider_models_rejects_empty_model_id() {
    let mut providers = HashMap::new();
    providers.insert(
        "openai-work".to_string(),
        JsonProviderConfig {
            base_url: "https://gateway.example/v1".to_string(),
            api_key: "key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: " ".to_string(),
                name: None,
                reasoning: None,
                input: None,
                cost: None,
                context_window: None,
                max_tokens: None,
                compat: None,
            }],
        },
    );

    let err = validate_json_provider_models(&JsonConfig {
        settings: None,
        models: Some(JsonModelsConfig {
            providers: Some(providers),
        }),
        agents: None,
        mcp_servers: None,
        s3: None,
    })
    .expect_err("empty model ids should be rejected");

    assert!(err.contains("model id cannot be empty"));
}

#[test]
fn validate_json_mcp_servers_rejects_zero_timeout() {
    let mut mcp_servers = HashMap::new();
    mcp_servers.insert(
        "zero-timeout".to_string(),
        JsonMcpServerConfig {
            command: "uvx".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            enabled: true,
            timeout_secs: Some(0),
        },
    );

    let err = Config::validate_json_mcp_servers(&JsonConfig {
        settings: None,
        models: None,
        agents: None,
        mcp_servers: Some(mcp_servers),
        s3: None,
    })
    .expect_err("zero MCP timeout should be rejected");

    assert!(err.contains("zero-timeout"));
    assert!(err.contains("greater than 0"));
}
