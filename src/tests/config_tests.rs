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
