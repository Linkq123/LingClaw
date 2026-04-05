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
        provider: crate::Provider::OpenAI,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: crate::DEFAULT_PORT,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        tool_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
        openai_stream_include_usage: false,
        structured_memory: false,
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
