use super::*;
use crate::{Provider, DEFAULT_PORT};
use std::{collections::HashMap, time::Duration};
use tokio::io::{AsyncWriteExt, BufReader};

fn test_config_with_mcp() -> Config {
    let mut mcp_servers = HashMap::new();
    mcp_servers.insert(
        "github".to_string(),
        JsonMcpServerConfig {
            command: "npx".to_string(),
            args: vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-github".to_string(),
            ],
            env: HashMap::new(),
            cwd: None,
            enabled: true,
            timeout_secs: Some(20),
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
fn sanitize_name_segment_normalizes_non_identifier_chars() {
    assert_eq!(sanitize_name_segment("GitHub Repo"), "github_repo");
    assert_eq!(sanitize_name_segment("123-server"), "t_123_server");
    assert_eq!(sanitize_name_segment("---"), "tool");
}

#[test]
fn build_exposed_name_adds_suffix_for_collisions() {
    let first = build_exposed_name("github", "list issues");
    let second = build_exposed_name("github", "list-issues");

    assert!(first.starts_with("mcp__github__list_issues__"));
    assert!(second.starts_with("mcp__github__list_issues__"));
    assert_ne!(first, second);
}

#[test]
fn build_exposed_name_stays_unique_for_sanitized_server_collisions() {
    let first = build_exposed_name("github-repo", "list issues");
    let second = build_exposed_name("github_repo", "list issues");

    assert!(first.starts_with("mcp__github_repo__list_issues__"));
    assert!(second.starts_with("mcp__github_repo__list_issues__"));
    assert_ne!(first, second);
}

#[test]
fn render_call_result_prefers_text_and_structured_content() {
    let rendered = render_call_result(&json!({
        "content": [
            {"type": "text", "text": "hello"},
            {"type": "resource", "uri": "file:///tmp/demo"}
        ],
        "structuredContent": {"ok": true}
    }));

    assert!(rendered.contains("hello"));
    assert!(rendered.contains("[resource]"));
    assert!(rendered.contains("structuredContent"));
}

#[test]
fn runtime_tool_note_lists_enabled_servers() {
    let note = runtime_tool_note(&test_config_with_mcp()).expect("note should exist");

    assert!(note.contains("github"));
    assert!(note.contains("mcp__"));
}

#[test]
fn server_timeout_defaults_to_tool_timeout_when_override_missing() {
    let mut config = test_config_with_mcp();
    config.exec_timeout = Duration::from_secs(7);
    config.tool_timeout = Duration::from_secs(45);
    config
        .mcp_servers
        .get_mut("github")
        .expect("github server should exist")
        .timeout_secs = None;

    let server = config
        .mcp_servers
        .get("github")
        .expect("github server should exist");

    assert_eq!(server_timeout_secs(server, &config), 45);
}

#[test]
fn resolve_server_cwd_rejects_workspace_escape() {
    let workspace = std::env::temp_dir().join("lingclaw-mcp-cwd-test");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let server = JsonMcpServerConfig {
        command: "npx".to_string(),
        args: vec![],
        env: HashMap::new(),
        cwd: Some("..".to_string()),
        enabled: true,
        timeout_secs: None,
    };

    let err = resolve_server_cwd(&server, &workspace).expect_err("workspace escape must fail");
    assert!(err.contains("outside the session workspace"));

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn format_mcp_timeout_error_includes_phase_and_diagnostics() {
    let error = format_mcp_timeout_error(
        "tools/list",
        120,
        &["Starting Minimax MCP server".to_string()],
        &["Traceback: missing key".to_string()],
    );

    assert!(error.contains("MCP tools/list timed out after 120s"));
    assert!(error.contains("stdout: Starting Minimax MCP server"));
    assert!(error.contains("stderr: Traceback: missing key"));
}

#[test]
fn push_diagnostic_line_trims_and_limits_buffer() {
    let mut lines = Vec::new();
    for index in 0..8 {
        push_diagnostic_line(&mut lines, &format!("line-{index}"));
    }

    assert_eq!(lines.len(), MCP_DIAGNOSTIC_LINE_LIMIT);
    assert_eq!(lines.first().map(String::as_str), Some("line-2"));
    assert_eq!(lines.last().map(String::as_str), Some("line-7"));
}

#[test]
fn write_message_uses_newline_delimited_jsonrpc() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let bytes = rt.block_on(async {
        write_message_for_test(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-03-26"}
        }))
        .await
        .expect("message should be written")
    });

    let output = String::from_utf8(bytes).expect("output should be utf-8");
    assert!(output.ends_with('\n'));
    assert!(!output.contains("Content-Length:"));
    assert!(output.trim_end().starts_with('{'));
}

#[test]
fn read_message_accepts_newline_delimited_jsonrpc_and_ignores_noise() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let message = rt.block_on(async {
        let (mut writer, reader) = tokio::io::duplex(512);
        let payload = json!({"jsonrpc": "2.0", "id": 1, "result": {"ok": true}}).to_string();
        let frame = format!("Starting Minimax MCP server\n{}\n", payload);
        let writer_task = tokio::spawn(async move {
            writer
                .write_all(frame.as_bytes())
                .await
                .expect("frame should be written");
        });
        let stdout_lines = Arc::new(Mutex::new(Vec::new()));
        let mut reader = BufReader::new(reader);
        let message = read_message(&mut reader, &stdout_lines)
            .await
            .expect("message should parse");
        writer_task.await.expect("writer task should finish");
        let diagnostics = snapshot_diagnostic_lines(&stdout_lines);
        (message, diagnostics)
    });

    assert_eq!(message.0.get("id").and_then(Value::as_u64), Some(1));
    assert_eq!(message.1, vec!["Starting Minimax MCP server".to_string()]);
}

#[test]
fn read_message_keeps_legacy_content_length_compatibility() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let message = rt.block_on(async {
        let (mut writer, reader) = tokio::io::duplex(512);
        let payload = json!({"jsonrpc": "2.0", "id": 2, "result": {"ok": true}}).to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{}", payload.len(), payload);
        let writer_task = tokio::spawn(async move {
            writer
                .write_all(frame.as_bytes())
                .await
                .expect("frame should be written");
        });
        let stdout_lines = Arc::new(Mutex::new(Vec::new()));
        let mut reader = BufReader::new(reader);
        let message = read_message(&mut reader, &stdout_lines)
            .await
            .expect("message should parse");
        writer_task.await.expect("writer task should finish");
        message
    });

    assert_eq!(message.get("id").and_then(Value::as_u64), Some(2));
}
