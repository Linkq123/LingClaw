use super::*;
use crate::{DEFAULT_PORT, Provider};
use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command as StdCommand,
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const MOCK_MCP_SERVER_SOURCE: &str = include_str!("fixtures/mock_mcp_server.rs");

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

fn unique_temp_workspace(prefix: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{unique}"))
}

fn mcp_test_guard() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn acquire_mcp_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
    mcp_test_guard().lock().await
}

fn mock_server_binary() -> &'static PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let helper_dir = std::env::temp_dir().join("lingclaw-mcp-test-helper");
        fs::create_dir_all(&helper_dir).expect("helper dir should exist");
        let source_path = helper_dir.join("mock_mcp_server.rs");
        let binary_path = helper_dir.join(if cfg!(windows) {
            "mock_mcp_server.exe"
        } else {
            "mock_mcp_server"
        });

        fs::write(&source_path, MOCK_MCP_SERVER_SOURCE).expect("helper source should write");
        let status = StdCommand::new("rustc")
            .arg("--edition=2021")
            .arg(&source_path)
            .arg("-o")
            .arg(&binary_path)
            .status()
            .expect("rustc should run");
        assert!(status.success(), "mock MCP server should compile");

        binary_path
    })
}

fn test_config_with_mock_server(mode: &str, log_path: &Path) -> Config {
    let mut config = test_config_with_mcp();
    config.mcp_servers.clear();
    config.mcp_servers.insert(
        "mock".to_string(),
        JsonMcpServerConfig {
            command: mock_server_binary().display().to_string(),
            args: Vec::new(),
            env: HashMap::from([
                ("LINGCLAW_MCP_MODE".to_string(), mode.to_string()),
                (
                    "LINGCLAW_MCP_LOG".to_string(),
                    log_path.display().to_string(),
                ),
            ]),
            cwd: None,
            enabled: true,
            timeout_secs: Some(5),
        },
    );
    config
}

async fn clear_mcp_caches_for_test() {
    if let Ok(mut cache) = tool_cache().lock() {
        cache.clear();
    }

    let sessions = {
        let Ok(mut cache) = session_cache().lock() else {
            return;
        };
        cache
            .drain()
            .map(|(_, entry)| entry.session)
            .collect::<Vec<_>>()
    };

    for session in sessions {
        let mut guard = session.lock().await;
        guard.shutdown().await;
    }
}

fn log_line_count(log_path: &Path, needle: &str) -> usize {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(needle))
        .count()
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

#[tokio::test]
async fn cached_tool_definitions_do_not_start_server_on_cache_miss() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-cache-miss");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("normal", &log_path);

    let tools = cached_tool_definitions_openai(&config, &workspace);
    let (cached_servers, enabled_servers) = cached_server_counts(&config, &workspace);

    assert!(tools.is_empty());
    assert_eq!(cached_servers, 0);
    assert_eq!(enabled_servers, 1);
    assert_eq!(log_line_count(&log_path, "tools/list"), 0);

    let _ = fs::remove_dir_all(&workspace);
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
fn should_reset_mcp_session_matches_transport_failures() {
    assert!(should_reset_mcp_session(
        "MCP initialize timed out after 5s"
    ));
    assert!(should_reset_mcp_session("MCP server closed stdout"));
    assert!(should_reset_mcp_session("failed to spawn 'npx': not found"));
    assert!(!should_reset_mcp_session(
        "{\"code\":-32602,\"message\":\"invalid args\"}"
    ));
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
fn resolve_server_command_falls_back_to_home_local_bin() {
    let temp_home = std::env::temp_dir().join("lingclaw-mcp-command-home-test");
    let local_bin = temp_home.join(".local").join("bin");
    std::fs::create_dir_all(&local_bin).expect("local bin should be created");

    let command_name = if cfg!(windows) { "uvx.exe" } else { "uvx" };
    let command_path = local_bin.join(command_name);
    std::fs::write(&command_path, b"echo test").expect("command file should be written");

    let resolved = resolve_server_command_from_env(
        "uvx",
        Some(OsString::from("")),
        Some(temp_home.clone().into_os_string()),
        None,
    );

    if cfg!(windows) {
        assert_eq!(
            resolved.to_string_lossy().to_ascii_lowercase(),
            command_path.to_string_lossy().to_ascii_lowercase()
        );
    } else {
        assert_eq!(resolved, command_path);
    }

    let _ = std::fs::remove_dir_all(&temp_home);
}

#[test]
fn resolve_server_command_keeps_explicit_paths() {
    let explicit = if cfg!(windows) {
        r"C:\tools\uvx.exe"
    } else {
        "/usr/local/bin/uvx"
    };

    let resolved = resolve_server_command_from_env(explicit, Some(OsString::from("")), None, None);

    assert_eq!(resolved, PathBuf::from(explicit));
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
            "params": {"protocolVersion": "2025-11-25"}
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

#[test]
fn read_response_handles_ping_requests_while_waiting_for_expected_id() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let result = rt.block_on(async {
        let (mut server_stdout, reader) = tokio::io::duplex(1024);
        let (mut client_stdin, server_stdin) = tokio::io::duplex(1024);
        let writer_task = tokio::spawn(async move {
            let ping = json!({"jsonrpc": "2.0", "id": "ping-1", "method": "ping"});
            let response = json!({"jsonrpc": "2.0", "id": 2, "result": {"tools": []}});
            server_stdout
                .write_all(format!("{}\n{}\n", ping, response).as_bytes())
                .await
                .expect("messages should be written");
        });

        let stdout_lines = Arc::new(Mutex::new(Vec::new()));
        let mut reader = BufReader::new(reader);
        let mut stdin_reader = BufReader::new(server_stdin);
        let response = read_response(
            &mut reader,
            &mut client_stdin,
            2,
            &stdout_lines,
            "github",
            Path::new("/tmp/workspace"),
            "cache-key",
        )
        .await
        .expect("expected response should be returned");

        let mut ping_reply = String::new();
        stdin_reader
            .read_line(&mut ping_reply)
            .await
            .expect("ping reply should be readable");
        writer_task.await.expect("writer task should finish");

        (
            response,
            ping_reply,
            snapshot_diagnostic_lines(&stdout_lines),
        )
    });

    assert_eq!(result.0.get("id").and_then(Value::as_u64), Some(2));
    assert!(result.1.contains("\"id\":\"ping-1\""));
    assert!(result.1.contains("\"result\":{}"));
    assert!(
        result
            .2
            .iter()
            .any(|line| line.contains("\"method\":\"ping\""))
    );
}

#[test]
fn read_response_handles_roots_list_requests_while_waiting_for_expected_id() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let result = rt.block_on(async {
        let (mut server_stdout, reader) = tokio::io::duplex(1024);
        let (mut client_stdin, server_stdin) = tokio::io::duplex(1024);
        let writer_task = tokio::spawn(async move {
            let roots_list = json!({"jsonrpc": "2.0", "id": 7, "method": "roots/list"});
            let response = json!({"jsonrpc": "2.0", "id": 2, "result": {"tools": []}});
            server_stdout
                .write_all(format!("{}\n{}\n", roots_list, response).as_bytes())
                .await
                .expect("messages should be written");
        });

        let stdout_lines = Arc::new(Mutex::new(Vec::new()));
        let workspace = if cfg!(windows) {
            PathBuf::from(r"C:\tmp\workspace root")
        } else {
            PathBuf::from("/tmp/workspace root")
        };
        let mut reader = BufReader::new(reader);
        let mut stdin_reader = BufReader::new(server_stdin);
        let response = read_response(
            &mut reader,
            &mut client_stdin,
            2,
            &stdout_lines,
            "github",
            &workspace,
            "cache-key",
        )
        .await
        .expect("expected response should be returned");

        let mut roots_reply = String::new();
        stdin_reader
            .read_line(&mut roots_reply)
            .await
            .expect("roots reply should be readable");
        writer_task.await.expect("writer task should finish");
        (response, roots_reply)
    });

    assert_eq!(result.0.get("id").and_then(Value::as_u64), Some(2));
    assert!(result.1.contains("\"id\":7"));
    assert!(result.1.contains("\"roots\""));
    assert!(result.1.contains("file://"));
}

#[tokio::test(flavor = "current_thread")]
async fn tools_list_changed_notification_invalidates_cached_descriptors() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-tool-change");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("tool-change", &log_path);

    let first = list_server_tools("mock", &config, &workspace)
        .await
        .expect("first tools/list should succeed");
    assert_eq!(first[0].raw_name, "alpha");

    call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {}}),
    )
    .await
    .expect("tools/call should consume invalidation notification");

    let second = list_server_tools("mock", &config, &workspace)
        .await
        .expect("second tools/list should refetch after invalidation");
    assert_eq!(second[0].raw_name, "beta");

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn call_server_restarts_cached_session_after_server_exit() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-restart");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("restart-once", &log_path);

    let first = call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {"value": "one"}}),
    )
    .await
    .expect("first tools/call should succeed");
    assert_eq!(first["content"][0]["text"], "ok");

    let second = call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {"value": "two"}}),
    )
    .await
    .expect("second tools/call should respawn session and succeed");
    assert_eq!(second["content"][0]["text"], "ok");
    assert_eq!(log_line_count(&log_path, "start"), 2);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn refresh_servers_clears_cached_tools_and_sessions() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-refresh");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("default", &log_path);

    let _ = list_server_tools("mock", &config, &workspace)
        .await
        .expect("tools should load");
    let _ = call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {}}),
    )
    .await
    .expect("session should be created");

    assert_eq!(tool_cache().lock().expect("tool cache lock").len(), 1);
    assert_eq!(session_cache().lock().expect("session cache lock").len(), 1);

    let reports = refresh_servers(&config, &workspace)
        .await
        .expect("refresh should succeed");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].server_name, "mock");
    assert_eq!(tool_cache().lock().expect("tool cache lock").len(), 1);
    assert_eq!(session_cache().lock().expect("session cache lock").len(), 0);
    assert_eq!(log_line_count(&log_path, "start"), 2);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn reap_idle_server_sessions_removes_stale_entries() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-idle");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("default", &log_path);

    let (cache_key, _) = get_or_create_server_session("mock", &config, &workspace)
        .await
        .expect("session should be created");
    {
        let mut cache = session_cache().lock().expect("session cache lock");
        let entry = cache
            .get_mut(&cache_key)
            .expect("cached session should exist");
        entry.last_used_at = Instant::now() - session_idle_ttl() - Duration::from_secs(1);
    }

    reap_idle_server_sessions(Instant::now())
        .await
        .expect("idle reap should succeed");
    assert_eq!(session_cache().lock().expect("session cache lock").len(), 0);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_calls_share_cached_session() {
    let _guard = acquire_mcp_test_guard().await;
    clear_mcp_caches_for_test().await;

    let workspace = unique_temp_workspace("lingclaw-mcp-concurrent");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let log_path = workspace.join("mock.log");
    let config = test_config_with_mock_server("concurrent", &log_path);

    call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {"value": "warmup"}}),
    )
    .await
    .expect("warmup call should succeed");
    assert_eq!(log_line_count(&log_path, "start"), 1);

    let left = call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {"value": "left"}}),
    );
    let right = call_server(
        "mock",
        &config,
        &workspace,
        "tools/call",
        json!({"name": "alpha", "arguments": {"value": "right"}}),
    );

    let (left, right) = tokio::join!(left, right);
    assert_eq!(
        left.expect("left call should succeed")["content"][0]["text"],
        "left"
    );
    assert_eq!(
        right.expect("right call should succeed")["content"][0]["text"],
        "right"
    );
    assert_eq!(log_line_count(&log_path, "start"), 1);

    clear_mcp_caches_for_test().await;
    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn inspect_servers_returns_reports_in_sorted_order() {
    let mut config = test_config_with_mcp();
    config.mcp_servers.insert(
        "alpha".to_string(),
        JsonMcpServerConfig {
            command: "definitely-not-a-real-command".to_string(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
            enabled: true,
            timeout_secs: Some(1),
        },
    );
    config
        .mcp_servers
        .get_mut("github")
        .expect("github server should exist")
        .command = "definitely-not-a-real-command".to_string();

    let workspace = std::env::temp_dir().join("lingclaw-mcp-inspect-order-test");
    std::fs::create_dir_all(&workspace).expect("workspace should be created");

    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let reports = rt.block_on(async { inspect_servers(&config, &workspace).await });

    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].server_name, "alpha");
    assert_eq!(reports[1].server_name, "github");

    let _ = std::fs::remove_dir_all(&workspace);
}

#[test]
fn path_to_file_uri_encodes_spaces_and_non_ascii() {
    let uri = path_to_file_uri(Path::new("/tmp/my workspace"));
    assert_eq!(uri, "file:///tmp/my%20workspace");

    let uri_cn = path_to_file_uri(Path::new("/home/用户/workspace"));
    assert!(uri_cn.starts_with("file:///home/"));
    assert!(
        !uri_cn.contains("用户"),
        "non-ASCII chars must be percent-encoded"
    );
    assert!(
        uri_cn.contains('%'),
        "non-ASCII bytes must be percent-encoded"
    );
}

#[test]
fn spawn_cooldown_blocks_rapid_retry() {
    let server = "test_cooldown_server";
    // Clear any existing state.
    clear_spawn_failure(server);
    assert!(check_spawn_cooldown(server).is_none());

    // Record failure and verify cooldown is active.
    record_spawn_failure(server);
    let remaining = check_spawn_cooldown(server);
    assert!(
        remaining.is_some(),
        "cooldown should be active after failure"
    );
    assert!(remaining.unwrap() > 0);

    // Clear and verify cooldown is gone.
    clear_spawn_failure(server);
    assert!(check_spawn_cooldown(server).is_none());
}
