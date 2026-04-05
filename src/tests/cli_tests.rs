use super::*;
use crate::{Provider, config::JsonMcpServerConfig};
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
        fast_model: None,
        sub_agent_model: None,
        memory_model: None,
        provider: Provider::OpenAI,
        openai_stream_include_usage: false,
        structured_memory: false,
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
    assert!(
        reports[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("failed to spawn 'definitely-not-a-real-command'")
    );
}

#[test]
fn preflight_config_caps_mcp_timeouts() {
    let mut config = test_config_with_broken_mcp();
    config.tool_timeout = Duration::from_secs(120);
    config
        .mcp_servers
        .get_mut("broken")
        .expect("broken server should exist")
        .timeout_secs = Some(90);

    let preflight = preflight_config(&config);
    assert_eq!(
        preflight.tool_timeout,
        Duration::from_secs(MCP_PREFLIGHT_TIMEOUT_SECS)
    );
    assert_eq!(
        preflight
            .mcp_servers
            .get("broken")
            .and_then(|server| server.timeout_secs),
        Some(MCP_PREFLIGHT_TIMEOUT_SECS)
    );
}

#[test]
fn preflight_config_sets_default_timeout_when_missing() {
    let mut config = test_config_with_broken_mcp();
    config
        .mcp_servers
        .get_mut("broken")
        .expect("broken server should exist")
        .timeout_secs = None;

    let preflight = preflight_config(&config);
    assert_eq!(
        preflight
            .mcp_servers
            .get("broken")
            .and_then(|server| server.timeout_secs),
        Some(MCP_PREFLIGHT_TIMEOUT_SECS)
    );
}

#[test]
fn with_preflight_timeout_returns_ready_result() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let value = rt
        .block_on(async { with_preflight_timeout(async { 7_u8 }).await })
        .expect("ready result should not time out");

    assert_eq!(value, 7);
}

#[test]
fn with_preflight_timeout_rejects_slow_future() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let err = rt
        .block_on(async {
            with_preflight_timeout(async {
                tokio::time::sleep(Duration::from_secs(MCP_PREFLIGHT_TIMEOUT_SECS + 1)).await;
            })
            .await
        })
        .expect_err("slow future should time out");

    assert!(err.contains("MCP preflight timed out after"));
}

#[test]
fn run_mcp_inspection_without_timeout_returns_reports() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let workspace = std::env::temp_dir().join("lingclaw-mcp-inspection-test");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let reports = rt
        .block_on(async {
            run_mcp_inspection(&test_config_with_broken_mcp(), &workspace, None).await
        })
        .expect("inspection should complete without wrapper timeout");

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].server_name, "broken");
    assert!(
        reports[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("failed to spawn")
    );

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn inspect_mcp_check_is_nonfatal_inside_runtime() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let reports = rt
        .block_on(async { inspect_mcp_check(&test_config_with_broken_mcp()) })
        .expect("mcp-check should return reports even when a server fails");

    assert_eq!(reports.len(), 1);
    assert!(
        reports[0]
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("failed to spawn")
    );
}

#[test]
fn mcp_check_succeeded_returns_false_when_any_server_fails() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let reports = rt
        .block_on(async { inspect_mcp_check(&test_config_with_broken_mcp()) })
        .expect("mcp-check should return reports even when a server fails");

    assert!(!mcp_check_succeeded(&reports));
}

#[test]
fn mcp_check_succeeded_returns_true_for_empty_reports() {
    assert!(mcp_check_succeeded(&[]));
}

#[test]
fn remote_cargo_toml_refs_include_main_and_master_fallbacks() {
    let refs = remote_cargo_toml_refs();

    assert!(refs.iter().any(|value| value == "origin/main:Cargo.toml"));
    assert!(refs.iter().any(|value| value == "origin/master:Cargo.toml"));
}

#[cfg(not(target_os = "windows"))]
#[test]
fn build_systemd_service_unit_quotes_paths_with_spaces() {
    let exe = std::path::Path::new("/tmp/LingClaw Bin/lingclaw");
    let working_dir = std::path::Path::new("/tmp/LingClaw Bin");
    let unit = build_systemd_service_unit(exe, working_dir, "demo", "/home/demo user");

    assert!(unit.contains("WorkingDirectory=\"/tmp/LingClaw Bin\""));
    assert!(unit.contains("Environment=\"HOME=/home/demo user\""));
    assert!(unit.contains("ExecStart=\"/tmp/LingClaw Bin/lingclaw\" --serve"));
}
