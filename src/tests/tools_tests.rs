use super::*;
use std::{collections::HashMap, time::Duration};

fn test_config() -> Config {
    Config {
        api_key: "env-key".to_string(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        fast_model: None,
        sub_agent_model: None,
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

// ── is_parallelizable_tool / is_read_only_tool / is_task_tool tests ─────────

#[test]
fn is_read_only_tool_covers_expected_set() {
    for name in &[
        "think",
        "read_file",
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

// ── validate_string_property pattern enforcement ────────────────────────────

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

// ── validate_array_property minItems/maxItems enforcement ───────────────────

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
