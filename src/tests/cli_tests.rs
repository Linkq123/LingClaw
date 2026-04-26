use super::*;
use crate::{
    Provider,
    config::{JsonMcpServerConfig, S3Config},
};
use serde_json::json;
use std::{collections::HashMap, path::Path, path::PathBuf, time::Duration};

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{stamp}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir should be created");
    dir
}

fn write_text_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent dir should be created");
    }
    std::fs::write(path, content).expect("file should be written");
}

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
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::OpenAI,
        openai_stream_include_usage: false,
        structured_memory: false,

        daily_reflection: false,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers,
        port: DEFAULT_PORT,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        tool_timeout: Duration::from_secs(30),
        sub_agent_timeout: Duration::from_secs(300),
        max_llm_retries: 2,
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
        s3: None,
    }
}

fn test_s3_config() -> Config {
    let mut config = test_config_with_broken_mcp();
    config.s3 = Some(S3Config {
        endpoint: "https://s3.us-east-1.amazonaws.com".to_string(),
        region: "us-east-1".to_string(),
        bucket: "demo-bucket".to_string(),
        access_key: "AKIAEXAMPLE".to_string(),
        secret_key: "secret".to_string(),
        prefix: "lingclaw/images/".to_string(),
        url_expiry_secs: 604_800,
        lifecycle_days: 14,
    });
    config
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
fn build_s3_start_detail_lines_show_enabled_summary() {
    let lines = build_s3_start_detail_lines(&test_s3_config(), true, Some(true), None);

    assert_eq!(lines[0], "  S3:      enabled");
    assert!(
        lines
            .iter()
            .any(|line| line.contains("https://s3.us-east-1.amazonaws.com"))
    );
    assert!(
        lines.iter().any(
            |line| line.contains("bucket=demo-bucket region=us-east-1 prefix=lingclaw/images/")
        )
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("presign=604800s lifecycle=14d"))
    );
}

#[test]
fn build_s3_start_detail_lines_report_disabled_by_setting() {
    let lines =
        build_s3_start_detail_lines(&test_config_with_broken_mcp(), true, Some(false), None);

    assert_eq!(lines[0], "  S3:      disabled by settings.enableS3=false");
    assert!(
        lines
            .iter()
            .any(|line| line.contains("s3 section exists but runtime uploads are disabled"))
    );
}

#[test]
fn build_s3_start_detail_lines_report_env_override_note() {
    let lines = build_s3_start_detail_lines(&test_s3_config(), true, Some(false), Some(true));

    assert!(lines.iter().any(|line| {
        line.contains("LINGCLAW_ENABLE_S3=true overrides settings.enableS3=false")
    }));
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

#[test]
fn wizard_next_provider_name_numbers_per_family() {
    let mut providers = serde_json::Map::new();
    providers.insert("openai".to_string(), json!({}));
    providers.insert("openai-2".to_string(), json!({}));
    providers.insert("anthropic".to_string(), json!({}));

    assert_eq!(
        wizard_next_provider_name(WizardProviderKind::OpenAI, &providers),
        "openai-3"
    );
    assert_eq!(
        wizard_next_provider_name(WizardProviderKind::Anthropic, &providers),
        "anthropic-2"
    );
    assert_eq!(
        wizard_next_provider_name(WizardProviderKind::Ollama, &providers),
        "ollama"
    );
}

#[test]
fn wizard_validate_provider_name_rejects_duplicates_and_invalid_chars() {
    let mut providers = serde_json::Map::new();
    providers.insert("openai".to_string(), json!({}));

    let duplicate_err = wizard_validate_provider_name("openai", &providers)
        .expect_err("duplicate provider names should fail");
    assert!(duplicate_err.contains("already exists"));

    let slash_err = wizard_validate_provider_name("openai/test", &providers)
        .expect_err("slashes should be rejected");
    assert!(slash_err.contains("cannot contain '/'"));

    let space_err = wizard_validate_provider_name("openai test", &providers)
        .expect_err("whitespace should be rejected");
    assert!(space_err.contains("whitespace"));

    wizard_validate_provider_name("openai_compat.2", &providers)
        .expect("letters, digits, dot, dash and underscore should be accepted");
}

#[test]
fn wizard_suggested_fast_model_prefers_known_family_fast_model() {
    let mut providers = serde_json::Map::new();
    providers.insert(
        "anthropic-2".to_string(),
        json!({
            "baseUrl": "https://anthropic-gateway.example",
            "apiKey": "sk-ant-test",
            "api": "anthropic",
            "models": [
                { "id": "claude-sonnet-4-20250514" },
                { "id": "claude-haiku-3-20250306" }
            ]
        }),
    );

    let fast_model =
        wizard_suggested_fast_model(&providers, Some("anthropic-2/claude-sonnet-4-20250514"))
            .expect("fast model should be suggested");

    assert_eq!(fast_model, "anthropic-2/claude-haiku-3-20250306");
}

#[test]
fn wizard_suggested_fast_model_falls_back_to_primary_model() {
    let mut providers = serde_json::Map::new();
    providers.insert(
        "openai-2".to_string(),
        json!({
            "baseUrl": "https://openai-gateway.example/v1",
            "apiKey": "sk-test",
            "api": "openai-completions",
            "models": [
                { "id": "gpt-4.1" }
            ]
        }),
    );

    let fast_model = wizard_suggested_fast_model(&providers, Some("openai-2/gpt-4.1"))
        .expect("primary model should be used when no family fast model is configured");

    assert_eq!(fast_model, "openai-2/gpt-4.1");
}

#[test]
fn prepare_frontend_assets_reuses_prebuilt_static_when_frontend_source_is_missing() {
    let source_dir = unique_temp_dir("lingclaw-install-static-only");
    write_text_file(
        &source_dir.join("static").join("index.html"),
        "<html>ok</html>",
    );

    let result = prepare_frontend_assets_with(&source_dir, "definitely-not-real-npm")
        .expect("existing static assets should be reusable");

    assert_eq!(result, FrontendPrepareResult::UsedPrebuiltStatic);

    let _ = std::fs::remove_dir_all(&source_dir);
}

#[test]
fn prepare_frontend_assets_reuses_prebuilt_static_when_npm_is_unavailable() {
    let source_dir = unique_temp_dir("lingclaw-install-no-npm");
    write_text_file(
        &source_dir.join("frontend").join("package.json"),
        "{\"name\":\"frontend\"}",
    );
    write_text_file(
        &source_dir.join("static").join("index.html"),
        "<html>ok</html>",
    );

    let result = prepare_frontend_assets_with(&source_dir, "definitely-not-real-npm")
        .expect("existing static assets should be reused when npm is missing");

    assert_eq!(result, FrontendPrepareResult::UsedPrebuiltStaticWithoutNpm);

    let _ = std::fs::remove_dir_all(&source_dir);
}

#[test]
fn prepare_frontend_assets_fails_without_npm_when_static_bundle_is_missing() {
    let source_dir = unique_temp_dir("lingclaw-install-missing-static");
    write_text_file(
        &source_dir.join("frontend").join("package.json"),
        "{\"name\":\"frontend\"}",
    );

    let error = prepare_frontend_assets_with(&source_dir, "definitely-not-real-npm").expect_err(
        "install should fail when frontend source exists but no static bundle can be produced",
    );

    assert!(error.to_string().contains("unavailable"));

    let _ = std::fs::remove_dir_all(&source_dir);
}

#[test]
fn install_frontend_assets_replaces_stale_target_static_dir() {
    let source_dir = unique_temp_dir("lingclaw-install-source-static");
    let install_dir = unique_temp_dir("lingclaw-install-target-static");

    write_text_file(
        &source_dir.join("static").join("index.html"),
        "<html>new</html>",
    );
    write_text_file(
        &source_dir.join("static").join("assets").join("app.js"),
        "console.log('new');",
    );
    write_text_file(&install_dir.join("static").join("stale.txt"), "stale");

    install_frontend_assets(&source_dir, &install_dir)
        .expect("frontend assets should copy cleanly");

    assert!(install_dir.join("static").join("index.html").is_file());
    assert!(
        install_dir
            .join("static")
            .join("assets")
            .join("app.js")
            .is_file()
    );
    assert!(!install_dir.join("static").join("stale.txt").exists());

    let _ = std::fs::remove_dir_all(&source_dir);
    let _ = std::fs::remove_dir_all(&install_dir);
}

#[test]
fn install_release_artifacts_copies_binary_and_frontend_assets() {
    let source_dir = unique_temp_dir("lingclaw-install-release-source");
    let install_dir = unique_temp_dir("lingclaw-install-release-target");
    let built_exe = release_binary_path(&source_dir);
    let current_exe = install_dir.join(if cfg!(windows) {
        "lingclaw.exe"
    } else {
        "lingclaw"
    });

    write_text_file(&built_exe, "new-binary");
    write_text_file(
        &source_dir.join("static").join("index.html"),
        "<html>new</html>",
    );
    write_text_file(
        &source_dir.join("static").join("assets").join("app.js"),
        "console.log('new');",
    );
    write_text_file(&current_exe, "old-binary");
    write_text_file(&install_dir.join("static").join("stale.txt"), "stale");

    install_release_artifacts(&source_dir, &built_exe, &current_exe)
        .expect("release artifacts should install cleanly");

    assert_eq!(
        std::fs::read_to_string(&current_exe).expect("installed binary should be readable"),
        "new-binary"
    );
    assert!(install_dir.join("static").join("index.html").is_file());
    assert!(
        install_dir
            .join("static")
            .join("assets")
            .join("app.js")
            .is_file()
    );
    assert!(!install_dir.join("static").join("stale.txt").exists());

    let _ = std::fs::remove_dir_all(&source_dir);
    let _ = std::fs::remove_dir_all(&install_dir);
}

#[test]
fn parse_version_triple_parses_major_minor_patch() {
    assert_eq!(parse_version_triple("1.85.0"), Some((1, 85, 0)));
    assert_eq!(parse_version_triple("2.0.1"), Some((2, 0, 1)));
}

#[test]
fn parse_version_triple_accepts_missing_patch() {
    // Some rustc builds omit the patch component.
    assert_eq!(parse_version_triple("1.85"), Some((1, 85, 0)));
}

#[test]
fn parse_version_triple_rejects_non_numeric() {
    assert_eq!(parse_version_triple(""), None);
    assert_eq!(parse_version_triple("abc"), None);
}

#[test]
fn doctor_node_version_strips_leading_v() {
    // detect_node_version() trims the leading 'v' from "v22.14.0".
    // Simulate that behaviour directly on the raw string.
    let raw = "v22.14.0\n";
    let trimmed = raw.trim().trim_start_matches('v').to_string();
    assert_eq!(trimmed, "22.14.0");
}

#[test]
fn doctor_npm_version_trims_whitespace() {
    let raw = "10.9.0\n";
    let trimmed = raw.trim().to_string();
    assert_eq!(trimmed, "10.9.0");
}
