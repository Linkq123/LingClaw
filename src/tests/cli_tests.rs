use super::*;
use crate::{config::JsonMcpServerConfig, Provider};
use std::{collections::HashMap, time::Duration};

fn test_config_with_broken_mcp() -> Config {
    let mut mcp_servers = HashMap::new();
    mcp_servers.insert(
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

    Config {
        api_key: "env-key".to_string(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        provider: Provider::OpenAI,
        openai_stream_include_usage: false,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers,
        port: DEFAULT_PORT,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        tool_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
    }
}

#[test]
fn inspect_mcp_preflight_is_nonfatal_inside_runtime() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let reports = rt
        .block_on(async { inspect_mcp_preflight(&test_config_with_broken_mcp()) })
        .expect("preflight should return reports instead of failing startup");

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].server_name, "broken");
    assert!(reports[0].tool_names.is_empty());
    assert!(reports[0]
        .error
        .as_deref()
        .unwrap_or_default()
        .contains("failed to spawn 'definitely-not-a-real-command'"));
}
