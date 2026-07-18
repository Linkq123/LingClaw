use super::*;
use crate::tools::exec::forward_live_chunk;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{collections::HashMap, time::Duration};

fn test_config() -> Config {
    Config {
        explicit_primary_model_configured: true,
        provider_catalog_declared: false,
        api_key: "env-key".to_string(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: crate::Provider::OpenAI,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: crate::DEFAULT_PORT,
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
        enable_state_digest: true,
        enable_task_plan: true,
    }
}

const ONE_PIXEL_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII=";

fn exec_test_workspace(name: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("lingclaw-{name}-{unique}"))
}

fn failing_exec_command() -> &'static str {
    if cfg!(windows) { "exit /b 7" } else { "exit 7" }
}

fn direct_exec_success_args() -> serde_json::Value {
    if cfg!(windows) {
        json!({
            "program": "cmd",
            "args": ["/C", "echo direct-ok"],
        })
    } else {
        json!({
            "program": "sh",
            "args": ["-c", "printf 'direct-ok'"],
        })
    }
}

fn direct_exec_env_stdin_args() -> serde_json::Value {
    if cfg!(windows) {
        json!({
            "program": "cmd",
            "args": ["/V:ON", "/C", "set /p INPUT= & echo !LC_TEST_ENV!:!INPUT!"],
            "env": {"LC_TEST_ENV": "from-env"},
            "stdin": "from-stdin\n",
        })
    } else {
        json!({
            "program": "sh",
            "args": ["-c", "read INPUT; printf '%s:%s' \"$LC_TEST_ENV\" \"$INPUT\""],
            "env": {"LC_TEST_ENV": "from-env"},
            "stdin": "from-stdin\n",
        })
    }
}

fn slow_exec_args() -> serde_json::Value {
    if cfg!(windows) {
        json!({
            "program": "cmd",
            "args": ["/C", "echo before & ping -n 3 127.0.0.1 >nul"],
        })
    } else {
        json!({
            "program": "sh",
            "args": ["-c", "printf 'before\\n'; sleep 2"],
        })
    }
}

fn stdout_burst_args(len: usize) -> serde_json::Value {
    let payload = "A".repeat(len);
    if cfg!(windows) {
        json!({
            "program": "cmd",
            "args": ["/C", format!("echo {payload}")],
        })
    } else {
        json!({
            "program": "sh",
            "args": ["-c", format!("printf '%s' '{payload}'")],
        })
    }
}

fn stdout_payload_args(payload: &str) -> serde_json::Value {
    if cfg!(windows) {
        json!({
            "program": "cmd",
            "args": ["/C", format!("echo {payload}")],
        })
    } else {
        json!({
            "program": "sh",
            "args": ["-c", format!("printf '%s' '{payload}'")],
        })
    }
}

fn stdout_and_stderr_payload_args(payload: &str) -> serde_json::Value {
    if cfg!(windows) {
        json!({
            "program": "cmd",
            "args": ["/C", format!("echo {payload} & echo {payload} 1>&2")],
        })
    } else {
        json!({
            "program": "sh",
            "args": ["-c", format!("printf '%s' '{payload}'; printf '%s' '{payload}' 1>&2")],
        })
    }
}

#[test]
fn validate_tool_args_rejects_non_object_arguments() {
    let schema = tool_parameters_read_file();
    let error = validate_tool_args("read_file", &json!("oops"), &schema)
        .expect("non-object arguments should be rejected");
    assert!(error.contains("arguments must be a JSON object"));
}

#[test]
fn validate_tool_args_rejects_wrong_type_and_out_of_range() {
    let search_schema = tool_parameters_search_files();
    let type_error = validate_tool_args(
        "search_files",
        &json!({"pattern": "todo", "max_results": "a lot"}),
        &search_schema,
    )
    .expect("wrong type should be rejected");
    assert!(type_error.contains("must be an integer"));

    let fetch_schema = tool_parameters_http_fetch();
    let range_error = validate_tool_args(
        "http_fetch",
        &json!({"url": "https://example.com", "max_bytes": 0}),
        &fetch_schema,
    )
    .expect("out-of-range value should be rejected");
    assert!(range_error.contains("must be >= 1"));
}

#[test]
fn validate_tool_args_rejects_unexpected_exec_parameter() {
    let schema = tool_parameters_exec();
    let error = validate_tool_args(
        "exec",
        &json!({"command": "echo ok", "api_key": "secret"}),
        &schema,
    )
    .expect("unexpected parameter should be rejected");
    assert!(error.contains("unexpected parameter 'api_key'"));
}

#[test]
fn validate_tool_args_rejects_non_string_exec_array_items_and_env_values() {
    let schema = tool_parameters_exec();
    let arg_error = validate_tool_args(
        "exec",
        &json!({"program": "git", "args": ["status", 3]}),
        &schema,
    )
    .expect("non-string array item should be rejected");
    assert!(arg_error.contains("parameter 'args[1]' must be a string"));

    let env_error = validate_tool_args(
        "exec",
        &json!({"program": "git", "env": {"TOKEN": 7}}),
        &schema,
    )
    .expect("non-string env value should be rejected");
    assert!(env_error.contains("parameter 'env.TOKEN' must be a string"));
}

#[test]
fn validate_tool_args_rejects_unexpected_orchestrate_task_field() {
    let schema = orchestrate_tool_parameters();
    let error = validate_tool_args(
        "orchestrate",
        &json!({
            "tasks": [{
                "id": "task-1",
                "agent": "general-purpose",
                "prompt": "Do the work.",
                "dependsOn": ["task-0"]
            }]
        }),
        &schema,
    )
    .expect("unexpected nested task parameter should be rejected");
    assert!(error.contains("parameter 'tasks[0].dependsOn' is not allowed"));
}

#[test]
fn validate_tool_args_rejects_missing_nested_required_orchestrate_task_fields() {
    let schema = orchestrate_tool_parameters();
    let error = validate_tool_args(
        "orchestrate",
        &json!({
            "tasks": [{
                "id": "task-1"
            }]
        }),
        &schema,
    )
    .expect("missing nested required fields should be rejected");
    assert!(error.contains("missing required parameter 'tasks[0].agent'"));
}

#[test]
fn gemini_tool_parameters_drop_unsupported_schema_keywords() {
    let schema = gemini_tool_parameters(json!({
        "type": "object",
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "additionalProperties": false,
        "properties": {
            "path": {
                "type": ["string", "null"],
                "description": "File path",
                "oneOf": [{ "const": "README.md" }]
            }
        },
        "required": ["path"]
    }));

    assert_eq!(schema["type"], "object");
    assert!(schema.get("$schema").is_none());
    assert!(schema.get("additionalProperties").is_none());
    assert_eq!(schema["properties"]["path"]["type"], "string");
    assert_eq!(schema["properties"]["path"]["nullable"], true);
    assert!(schema["properties"]["path"].get("oneOf").is_none());
}

#[tokio::test]
async fn execute_tool_rejects_descending_read_file_range() {
    let workspace = std::env::temp_dir().join("lingclaw-tools-range-test");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");
    tokio::fs::write(workspace.join("notes.txt"), "one\ntwo\nthree\nfour\n")
        .await
        .expect("fixture should be written");

    let outcome = execute_tool(
        "read_file",
        r#"{"path":"notes.txt","start_line":4,"end_line":2}"#,
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        None,
    )
    .await;

    assert!(outcome.is_error);
    assert!(
        outcome
            .output
            .contains("end_line must be greater than or equal to start_line")
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_shares_batch_image_budget_with_view_image() {
    let workspace = exec_test_workspace("view-image-budget");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");
    tokio::fs::write(
        workspace.join("pixel.png"),
        STANDARD.decode(ONE_PIXEL_PNG).expect("valid PNG fixture"),
    )
    .await
    .expect("fixture should be written");
    let budget = mcp::ToolImageBudget::new(1);

    let first = execute_tool_with_image_budget(
        TOOL_NAME_VIEW_IMAGE,
        r#"{"path":"pixel.png"}"#,
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        None,
        Some(budget.for_call(0)),
    )
    .await;
    let second = execute_tool_with_image_budget(
        TOOL_NAME_VIEW_IMAGE,
        r#"{"path":"pixel.png"}"#,
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        None,
        Some(budget.for_call(1)),
    )
    .await;

    assert!(!first.is_error);
    assert_eq!(first.images.len(), 1);
    assert!(!second.is_error);
    assert!(second.images.is_empty());
    assert!(second.output.contains("tool image batch limit reached"));

    let _ = tokio::fs::remove_dir_all(workspace).await;
}

#[tokio::test]
async fn execute_tool_rejects_zero_search_results_limit() {
    let workspace = std::env::temp_dir().join("lingclaw-tools-search-limit");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let outcome = execute_tool(
        "search_files",
        r#"{"pattern":"todo","max_results":0}"#,
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        None,
    )
    .await;

    assert!(outcome.is_error);
    assert!(
        outcome
            .output
            .contains("parameter 'max_results' must be >= 1")
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_marks_non_zero_exec_exit_as_error() {
    let workspace = exec_test_workspace("exec-nonzero");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let outcome = execute_tool(
        "exec",
        &serde_json::to_string(&json!({ "command": failing_exec_command() }))
            .expect("args should serialize"),
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        None,
    )
    .await;

    assert!(outcome.is_error, "non-zero exit should be marked as error");
    assert!(
        outcome
            .output
            .contains("exec error: command exited with code 7")
    );
    assert!(outcome.output.contains("exit code: 7"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_marks_blocked_exec_as_error() {
    let workspace = exec_test_workspace("exec-blocked");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let outcome = execute_tool(
        "exec",
        r#"{"command":"rm -rf /"}"#,
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        None,
    )
    .await;

    assert!(
        outcome.is_error,
        "blocked command should be marked as error"
    );
    assert!(outcome.output.starts_with("BLOCKED:"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_supports_direct_exec_mode() {
    let workspace = exec_test_workspace("exec-direct");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let outcome = execute_tool(
        "exec",
        &serde_json::to_string(&direct_exec_success_args()).expect("args should serialize"),
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        None,
    )
    .await;

    assert!(!outcome.is_error, "direct exec should succeed");
    assert!(outcome.output.contains("direct-ok"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[test]
fn forward_live_chunk_preserves_utf8_split_across_reads() {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut pending = Vec::new();
    let bytes = "你好".as_bytes();

    forward_live_chunk("stdout", &bytes[..2], &mut pending, Some(tx.clone()), None);
    assert!(
        rx.try_recv().is_err(),
        "incomplete utf-8 prefix should not emit yet"
    );
    assert_eq!(pending, bytes[..2]);

    forward_live_chunk("stdout", &bytes[2..], &mut pending, Some(tx), None);
    let mut combined = String::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            ToolLiveEvent::ExecOutput { chunk, .. } => combined.push_str(&chunk),
        }
    }

    assert_eq!(combined, "你好");
    assert!(pending.is_empty());
}

#[tokio::test]
async fn execute_tool_keeps_utf8_live_output_within_byte_budget() {
    let workspace = exec_test_workspace("exec-live-utf8-budget");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let mut config = test_config();
    config.max_output_bytes = 4;

    let script = if cfg!(windows) {
        json!({
            "program": "python",
            "args": ["-c", "import sys; sys.stdout.buffer.write('你好'.encode('utf-8')); sys.stdout.flush()"],
        })
    } else {
        json!({
            "program": "python3",
            "args": ["-c", "import sys; sys.stdout.buffer.write('你好'.encode('utf-8')); sys.stdout.flush()"],
        })
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = execute_tool(
        "exec",
        &serde_json::to_string(&script).expect("args should serialize"),
        &config,
        &reqwest::Client::new(),
        &workspace,
        Some(tx),
    )
    .await;

    assert!(!outcome.is_error, "utf8 live-output exec should succeed");
    let mut combined = String::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            ToolLiveEvent::ExecOutput { chunk, .. } => combined.push_str(&chunk),
        }
    }
    assert!(combined.len() <= config.max_output_bytes);
    assert_eq!(combined, "你");

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_emits_complete_split_utf8_live_output() {
    let workspace = exec_test_workspace("exec-live-split-utf8");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let script = if cfg!(windows) {
        json!({
            "program": "python",
            "args": [
                "-c",
                "import sys, time; sys.stdout.buffer.write('你'.encode('utf-8')); sys.stdout.flush(); time.sleep(0.1); sys.stdout.buffer.write('好'.encode('utf-8')); sys.stdout.flush()"
            ],
        })
    } else {
        json!({
            "program": "python3",
            "args": [
                "-c",
                "import sys, time; sys.stdout.buffer.write('你'.encode('utf-8')); sys.stdout.flush(); time.sleep(0.1); sys.stdout.buffer.write('好'.encode('utf-8')); sys.stdout.flush()"
            ],
        })
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = execute_tool(
        "exec",
        &serde_json::to_string(&script).expect("args should serialize"),
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        Some(tx),
    )
    .await;

    assert!(!outcome.is_error, "split utf-8 exec should succeed");
    assert!(outcome.output.contains("你好"));

    let mut combined = String::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            ToolLiveEvent::ExecOutput { chunk, .. } => combined.push_str(&chunk),
        }
    }

    assert!(combined.contains("你好"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_emits_live_exec_output_events() {
    let workspace = exec_test_workspace("exec-live-output");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = execute_tool(
        "exec",
        &serde_json::to_string(&direct_exec_success_args()).expect("args should serialize"),
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        Some(tx),
    )
    .await;

    assert!(!outcome.is_error, "direct exec should succeed");
    let mut chunks = Vec::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            ToolLiveEvent::ExecOutput { chunk, .. } => chunks.push(chunk),
        }
    }
    assert!(
        chunks.iter().any(|chunk| chunk.contains("direct-ok")),
        "live output should include command stdout"
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_supports_exec_env_and_stdin() {
    let workspace = exec_test_workspace("exec-env-stdin");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let outcome = execute_tool(
        "exec",
        &serde_json::to_string(&direct_exec_env_stdin_args()).expect("args should serialize"),
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        None,
    )
    .await;

    assert!(!outcome.is_error, "exec with env/stdin should succeed");
    assert!(outcome.output.contains("from-env:from-stdin"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_rejects_mixed_exec_modes() {
    let workspace = exec_test_workspace("exec-mixed-modes");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let outcome = execute_tool(
        "exec",
        r#"{"command":"echo hi","program":"cmd"}"#,
        &test_config(),
        &reqwest::Client::new(),
        &workspace,
        None,
    )
    .await;

    assert!(outcome.is_error);
    assert!(outcome.output.contains("use either 'command' or 'program'"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_reports_exec_timeout_with_partial_output() {
    let workspace = exec_test_workspace("exec-timeout");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let mut config = test_config();
    config.exec_timeout = Duration::from_millis(200);

    let outcome = execute_tool(
        "exec",
        &serde_json::to_string(&slow_exec_args()).expect("args should serialize"),
        &config,
        &reqwest::Client::new(),
        &workspace,
        None,
    )
    .await;

    assert!(outcome.is_error);
    assert!(outcome.output.contains("command timed out"));
    assert!(outcome.output.contains("--- stdout ---"));
    assert!(outcome.output.contains("before"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_caps_live_exec_output_to_budget() {
    let workspace = exec_test_workspace("exec-live-budget");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let mut config = test_config();
    config.max_output_bytes = 512;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome = execute_tool(
        "exec",
        &serde_json::to_string(&stdout_burst_args(1_500)).expect("args should serialize"),
        &config,
        &reqwest::Client::new(),
        &workspace,
        Some(tx),
    )
    .await;

    assert!(!outcome.is_error, "bounded live-output exec should succeed");
    let mut combined = String::new();
    while let Ok(event) = rx.try_recv() {
        match event {
            ToolLiveEvent::ExecOutput { chunk, .. } => combined.push_str(&chunk),
        }
    }
    assert!(
        combined.len() <= config.max_output_bytes,
        "live output should respect max_output_bytes"
    );
    assert!(
        !combined.is_empty(),
        "live output should still forward initial bytes"
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_uses_full_capture_budget_for_single_stream_output() {
    let workspace = exec_test_workspace("exec-single-stream-budget");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let mut config = test_config();
    config.max_output_bytes = 640;
    let payload = "A".repeat(240);
    let outcome = execute_tool(
        "exec",
        &serde_json::to_string(&stdout_burst_args(payload.len())).expect("args should serialize"),
        &config,
        &reqwest::Client::new(),
        &workspace,
        None,
    )
    .await;

    assert!(!outcome.is_error, "stdout-only exec should succeed");
    assert!(
        outcome.output.contains(&payload),
        "stdout-only exec should use the full shared capture budget"
    );

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_keeps_recent_tail_when_capture_budget_is_exceeded() {
    let workspace = exec_test_workspace("exec-tail-capture-budget");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let mut config = test_config();
    config.max_output_bytes = 320;
    let payload = format!("{}TAIL-MARKER", "A".repeat(400));
    let outcome = execute_tool(
        "exec",
        &serde_json::to_string(&stdout_payload_args(&payload)).expect("args should serialize"),
        &config,
        &reqwest::Client::new(),
        &workspace,
        None,
    )
    .await;

    assert!(!outcome.is_error, "stdout-only exec should succeed");
    let stdout_section = outcome
        .output
        .split("--- stdout ---\n")
        .nth(1)
        .and_then(|rest| rest.split("\n--- stderr ---").next())
        .expect("stdout section should exist");
    assert!(stdout_section.contains("[truncated]"));
    assert!(stdout_section.contains("TAIL-MARKER"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

#[tokio::test]
async fn execute_tool_caps_combined_stdout_and_stderr_output_to_budget() {
    let workspace = exec_test_workspace("exec-combined-stream-budget");
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    tokio::fs::create_dir_all(&workspace)
        .await
        .expect("workspace should be created");

    let mut config = test_config();
    config.max_output_bytes = 320;
    let payload = "B".repeat(220);
    let outcome = execute_tool(
        "exec",
        &serde_json::to_string(&stdout_and_stderr_payload_args(&payload))
            .expect("args should serialize"),
        &config,
        &reqwest::Client::new(),
        &workspace,
        None,
    )
    .await;

    assert!(!outcome.is_error, "dual-stream exec should succeed");
    assert!(
        outcome.output.len() <= config.max_output_bytes,
        "combined exec output should respect max_output_bytes"
    );
    assert!(outcome.output.contains("--- stdout ---"));
    assert!(outcome.output.contains("--- stderr ---"));
    assert!(outcome.output.contains("[truncated]"));

    let _ = tokio::fs::remove_dir_all(&workspace).await;
}

// 鈹€鈹€ is_parallelizable_tool / is_read_only_tool / is_task_tool tests 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn is_read_only_tool_covers_expected_set() {
    for name in &[
        "think",
        "read_file",
        "view_image",
        "list_dir",
        "search_files",
        "http_fetch",
    ] {
        assert!(is_read_only_tool(name), "{name} should be read-only");
    }
    for name in &["exec", "write_file", "patch_file", "delete_file", "task"] {
        assert!(!is_read_only_tool(name), "{name} should NOT be read-only");
    }
}

#[test]
fn is_task_tool_only_matches_task() {
    assert!(is_task_tool("task"));
    assert!(!is_task_tool("exec"));
    assert!(!is_task_tool("read_file"));
    assert!(!is_task_tool("task_like"));
}

#[test]
fn is_parallelizable_tool_matches_read_only_tools_only() {
    // All read-only tools should be parallelizable.
    for name in &[
        "think",
        "read_file",
        "view_image",
        "list_dir",
        "search_files",
        "http_fetch",
    ] {
        assert!(
            is_parallelizable_tool(name),
            "{name} should be parallelizable"
        );
    }
    // task stays sequential because sub-agents share the parent workspace.
    assert!(!is_parallelizable_tool("task"));
    // Write/exec tools are NOT parallelizable.
    for name in &["exec", "write_file", "patch_file", "delete_file", "task"] {
        assert!(
            !is_parallelizable_tool(name),
            "{name} should NOT be parallelizable"
        );
    }
}

// 鈹€鈹€ validate_string_property pattern enforcement 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn validate_tool_args_enforces_string_pattern() {
    let schema = json!({
        "type": "object",
        "properties": {
            "id": {
                "type": "string",
                "pattern": "^[A-Za-z0-9_-]+$"
            }
        },
        "required": ["id"]
    });

    // Valid value
    assert!(validate_tool_args("test", &json!({"id": "my-task_1"}), &schema).is_none());

    // Invalid value (contains space)
    let err = validate_tool_args("test", &json!({"id": "bad task"}), &schema)
        .expect("space should violate pattern");
    assert!(err.contains("does not match pattern"));

    // Invalid value (contains special chars)
    let err = validate_tool_args("test", &json!({"id": "bad@task!"}), &schema)
        .expect("special chars should violate pattern");
    assert!(err.contains("does not match pattern"));
}

// 鈹€鈹€ validate_array_property minItems/maxItems enforcement 鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€鈹€

#[test]
fn validate_tool_args_enforces_array_min_items() {
    let schema = json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "minItems": 1,
                "maxItems": 3
            }
        },
        "required": ["items"]
    });

    // Valid
    assert!(validate_tool_args("test", &json!({"items": [1]}), &schema).is_none());
    assert!(validate_tool_args("test", &json!({"items": [1, 2, 3]}), &schema).is_none());

    // Too few
    let err = validate_tool_args("test", &json!({"items": []}), &schema)
        .expect("empty array should fail minItems");
    assert!(err.contains("at least 1 items"));

    // Too many
    let err = validate_tool_args("test", &json!({"items": [1, 2, 3, 4]}), &schema)
        .expect("4 items should fail maxItems");
    assert!(err.contains("at most 3 items"));
}

#[test]
fn validate_tool_args_rejects_non_array_for_array_type() {
    let schema = json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array"
            }
        },
        "required": ["items"]
    });
    let err = validate_tool_args("test", &json!({"items": "not-array"}), &schema)
        .expect("string should be rejected for array type");
    assert!(err.contains("must be an array"));
}

#[test]
fn render_tool_prompt_lines_with_query_compresses_irrelevant_tools() {
    let rendered = render_tool_prompt_lines_with_query(
        &test_config(),
        Some("benchmark and profile build performance"),
    );

    assert!(rendered.contains("**exec**"));
    assert!(rendered.contains("**think**"));
    assert!(rendered.contains("Other available tools:"));
    assert!(!rendered.contains("view_image"));
}

#[test]
fn view_image_is_only_added_to_definitions_and_prompts_conditionally() {
    let default_openai = tool_definitions_openai();
    assert!(!default_openai.to_string().contains(TOOL_NAME_VIEW_IMAGE));
    assert!(
        !tool_definitions_anthropic()
            .to_string()
            .contains(TOOL_NAME_VIEW_IMAGE)
    );
    assert!(
        !tool_definitions_gemini()
            .to_string()
            .contains(TOOL_NAME_VIEW_IMAGE)
    );

    for provider in [
        Provider::OpenAI,
        Provider::OpenAIResponses,
        Provider::Anthropic,
        Provider::Ollama,
        Provider::Gemini,
    ] {
        let definition = conditional_view_image_definition(provider);
        assert!(definition.to_string().contains(TOOL_NAME_VIEW_IMAGE));
    }

    let without = render_read_only_tool_prompt_lines_with_view_image(&test_config(), false);
    let with = render_read_only_tool_prompt_lines_with_view_image(&test_config(), true);
    assert!(!without.contains(TOOL_NAME_VIEW_IMAGE));
    assert!(with.contains(TOOL_NAME_VIEW_IMAGE));
}

#[test]
fn available_builtin_tool_catalog_follows_view_image_capabilities() {
    let mut config = test_config();
    let model = "openai/vision-model";

    let names = available_builtin_tool_specs(&config, Some(model))
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    assert!(names.contains(&TOOL_NAME_READ_FILE));
    assert!(!names.contains(&TOOL_NAME_VIEW_IMAGE));

    config.s3 = Some(crate::config::S3Config {
        endpoint: "https://storage.example.test".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "images/".into(),
        url_expiry_secs: 3600,
        lifecycle_days: 14,
    });
    assert!(
        !available_builtin_tool_specs(&config, Some(model))
            .into_iter()
            .any(|spec| spec.name == TOOL_NAME_VIEW_IMAGE)
    );

    config.providers.insert(
        "openai".into(),
        crate::config::JsonProviderConfig {
            base_url: "https://api.openai.com/v1".into(),
            api_key: "test-key".into(),
            api: "openai-completions".into(),
            models: vec![crate::config::JsonModelEntry {
                id: "vision-model".into(),
                input: Some(vec!["text".into(), "image".into()]),
                ..Default::default()
            }],
        },
    );
    assert!(
        available_builtin_tool_specs(&config, Some(model))
            .into_iter()
            .any(|spec| spec.name == TOOL_NAME_VIEW_IMAGE)
    );
    assert!(
        !available_builtin_tool_specs(&config, None)
            .into_iter()
            .any(|spec| spec.name == TOOL_NAME_VIEW_IMAGE)
    );
}

#[test]
fn render_ranked_tool_recommendations_respects_memory_preferences() {
    let rendered = render_ranked_tool_recommendations(
        &test_config(),
        None,
        &ToolRankingContext {
            preferred_tools: vec!["exec".into(), "read_file".into(), "search_files".into()],
            preferences: Vec::new(),
        },
    )
    .expect("memory-aware tool ranking should render");

    assert!(rendered.starts_with("## Suggested Tool Order"));
    assert!(rendered.contains("1. **exec**"));
    assert!(rendered.contains("**read_file**"));
    assert!(rendered.contains("**search_files**"));
}

#[test]
fn tool_runtime_timeout_exempts_exec_and_todos() {
    let config = test_config();

    assert_eq!(tool_runtime_timeout(TOOL_NAME_EXEC, &config), None);
    assert_eq!(tool_runtime_timeout(TOOL_NAME_TODOS, &config), None);
    assert_eq!(
        tool_runtime_timeout(TOOL_NAME_READ_FILE, &config),
        Some(config.tool_timeout)
    );
}

#[test]
fn ensure_think_tool_preserves_rank_order_when_inserted() {
    let specs = tool_specs();
    let find_idx = |name: &str| {
        specs
            .iter()
            .position(|spec| spec.name == name)
            .expect("tool should exist")
    };
    let think_idx = find_idx(TOOL_NAME_THINK);
    let exec_idx = find_idx(TOOL_NAME_EXEC);
    let read_idx = find_idx(TOOL_NAME_READ_FILE);
    let search_idx = find_idx(TOOL_NAME_SEARCH_FILES);
    let list_idx = find_idx(TOOL_NAME_LIST_DIR);
    let write_idx = find_idx(TOOL_NAME_WRITE_FILE);

    let ranked_indices = vec![
        exec_idx, think_idx, read_idx, search_idx, list_idx, write_idx,
    ];
    let mut selected = vec![exec_idx, read_idx, search_idx, list_idx, write_idx];
    ensure_think_tool(specs, &ranked_indices, &mut selected);

    assert_eq!(
        selected,
        vec![exec_idx, think_idx, read_idx, search_idx, list_idx]
    );
}

#[test]
fn build_tool_execution_trace_covers_builtins_and_special_tools() {
    let exec = build_tool_execution_trace(
        TOOL_NAME_EXEC,
        Some(r#"{"command":"cargo test --workspace","working_dir":"crates/core"}"#),
    )
    .expect("exec trace should exist");
    assert_eq!(
        exec.summary(),
        Some("run `cargo test --workspace` in `crates/core`")
    );
    assert_eq!(exec.command.as_deref(), Some("cargo test --workspace"));

    let task = build_tool_execution_trace(
        TOOL_NAME_TASK,
        Some(r#"{"agent":"reviewer","prompt":"Inspect the failure"}"#),
    )
    .expect("task trace should exist");
    assert_eq!(task.summary(), Some("delegate to `reviewer`"));
    assert_eq!(task.agent.as_deref(), Some("reviewer"));

    let orchestrate = build_tool_execution_trace(
        TOOL_NAME_ORCHESTRATE,
        Some(r#"{"tasks":[{"id":"a","agent":"reviewer","prompt":"one"},{"id":"b","agent":"benchmarker","prompt":"two"}]}"#),
    )
    .expect("orchestrate trace should exist");
    assert_eq!(orchestrate.summary(), Some("orchestrate 2 delegated tasks"));
    assert_eq!(orchestrate.task_count, Some(2));
}

#[test]
fn build_tool_execution_trace_supports_direct_exec_mode() {
    let exec = build_tool_execution_trace(
        TOOL_NAME_EXEC,
        Some(r#"{"program":"cargo","args":["test","--workspace"],"working_dir":"frontend"}"#),
    )
    .expect("exec trace should exist");
    assert_eq!(
        exec.summary(),
        Some("run `cargo test --workspace` in `frontend`")
    );
    assert_eq!(exec.command.as_deref(), Some("cargo test --workspace"));
}

#[test]
fn display_tool_arguments_redacts_exec_secrets() {
    let rendered = display_tool_arguments(
        TOOL_NAME_EXEC,
        r#"{"command":"curl -H \"Authorization: Bearer super-secret\" --api-key \"key-123\" TOKEN=\"value\" --secret 'quoted-secret'","working_dir":"src"}"#,
    );

    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("super-secret"));
    assert!(!rendered.contains("key-123"));
    assert!(!rendered.contains("TOKEN=\"value\""));
    assert!(!rendered.contains("quoted-secret"));

    let rendered = display_tool_arguments(
        TOOL_NAME_EXEC,
        r#"{"command":"echo ok","apiKey":"key-123","nested":{"access_token":"token-456"},"notes":"Authorization: Bearer super-secret"}"#,
    );
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("key-123"));
    assert!(!rendered.contains("token-456"));
    assert!(!rendered.contains("super-secret"));

    let rendered = display_tool_arguments(
        TOOL_NAME_EXEC,
        r#"{"stdin":"sk-live-1234567890abcdefghijklmnop","body":{"token":"ghp_supersecretvalue","note":"keep this note"}}"#,
    );
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("sk-live-1234567890abcdefghijklmnop"));
    assert!(!rendered.contains("ghp_supersecretvalue"));
    assert!(rendered.contains("keep this note"));

    let rendered = display_tool_arguments(
        TOOL_NAME_EXEC,
        r#""curl -H \"Authorization: Bearer super-secret\" --api-key \"key-123\"""#,
    );
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("super-secret"));
    assert!(!rendered.contains("key-123"));

    let rendered =
        display_tool_arguments(TOOL_NAME_EXEC, r#""sk-live-1234567890abcdefghijklmnop""#);
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("sk-live-1234567890abcdefghijklmnop"));

    let sha_like = "0123456789abcdef0123456789abcdef01234567";
    let rendered = display_tool_arguments(TOOL_NAME_EXEC, &format!(r#""{sha_like}""#));
    assert!(!rendered.contains("[REDACTED]"));
    assert!(rendered.contains(sha_like));

    let rendered = display_tool_arguments(
        TOOL_NAME_EXEC,
        r#"{"program":"curl","args":["-H","Authorization: Bearer super-secret","--api-key","key-123","--token","token-456","--password","quoted-secret","https://example.com"]}"#,
    );
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("super-secret"));
    assert!(!rendered.contains("key-123"));
    assert!(!rendered.contains("token-456"));
    assert!(!rendered.contains("quoted-secret"));
    assert!(rendered.contains("https://example.com"));
}

#[test]
fn display_tool_arguments_redacts_create_session_profiles() {
    let rendered = display_tool_arguments(
        TOOL_NAME_SESSION_CONTROL,
        r#"{"action":"create_session","name":"Reviewer","purpose":"contains API_KEY=secret","identity_profile":"token is abc","user_profile":"private user context","style_profile":"concise","agent_notes":"Bearer super-secret","group_id":"group-a"}"#,
    );

    assert!(rendered.contains(r#""action":"create_session""#));
    assert!(rendered.contains(r#""name":"Reviewer""#));
    assert!(rendered.contains(r#""group_id":"group-a""#));
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("API_KEY=secret"));
    assert!(!rendered.contains("token is abc"));
    assert!(!rendered.contains("private user context"));
    assert!(!rendered.contains("Bearer super-secret"));

    let rendered = display_tool_arguments(
        TOOL_NAME_SESSION_CONTROL,
        r#"{"action":"create_session","api_key":"sk-live-1234567890abcdefghijklmnop","meta":{"token":"ghp_supersecretvalue"},"name":"Reviewer"}"#,
    );
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("sk-live-1234567890abcdefghijklmnop"));
    assert!(!rendered.contains("ghp_supersecretvalue"));

    let rendered = display_tool_arguments(
        TOOL_NAME_SESSION_CONTROL,
        r#"{"action":"Create_Session","identity_profile":"private user context","agent_notes":"Bearer super-secret"}"#,
    );
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("private user context"));
    assert!(!rendered.contains("super-secret"));

    let rendered = display_tool_arguments(
        TOOL_NAME_SESSION_CONTROL,
        r#"{"action":"create_session","payload":{"Identity_Profile":"private nested context","items":[{"agent_notes":"nested secret"}]}}"#,
    );
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("private nested context"));
    assert!(!rendered.contains("nested secret"));

    let rendered = display_tool_arguments(
        TOOL_NAME_SESSION_CONTROL,
        r#"{"action":"create_session","agent_notes":"Bearer super-secret""#,
    );

    assert!(rendered.contains(r#""action":"<unknown>""#));
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("super-secret"));

    let rendered = display_tool_arguments(
        TOOL_NAME_SESSION_CONTROL,
        r#"{"identity_profile":"private user context""#,
    );
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("private user context"));
    assert!(rendered.contains(r#""action":"<unknown>""#));

    let rendered = display_tool_arguments(
        TOOL_NAME_SESSION_CONTROL,
        r#""create_session agent_notes Bearer super-secret""#,
    );
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("super-secret"));

    let rendered = display_tool_arguments(
        TOOL_NAME_SESSION_CONTROL,
        r#"["create_session","agent_notes","Bearer super-secret"]"#,
    );
    assert!(rendered.contains(r#""agent_notes""#));
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("super-secret"));

    let rendered = display_tool_arguments(
        TOOL_NAME_SESSION_CONTROL,
        r#"["create_session","agent_notes",{"comment":"x"},"Bearer hunter2"]"#,
    );
    assert!(rendered.contains(r#""agent_notes""#));
    assert!(rendered.contains("[REDACTED]"));
    assert!(!rendered.contains("hunter2"));
}
