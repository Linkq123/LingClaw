use crate::subagents::discovery::discover_all_agents;
use crate::subagents::orchestrator::{
    OrchestrationOutcome, OrchestrationPlan, OrchestrationTask, TaskResult, TaskStatus,
    compute_execution_layers, execute_orchestration, format_orchestration_result, has_cycle,
    interpolate_results, validate_plan,
};
use crate::subagents::{AgentSource, SubAgentSpec, ToolPermissions, render_agents_catalog};
use crate::{ChatMessage, agent};
use tokio_util::sync::CancellationToken;

#[test]
fn test_tool_permissions_basic() {
    let perms = ToolPermissions {
        allow: vec![],
        deny: vec![],
    };
    // Empty allow = all tools allowed (except task)
    assert!(perms.is_allowed("read_file"));
    assert!(perms.is_allowed("exec"));
    assert!(!perms.is_allowed("task")); // task always denied
}

#[test]
fn test_tool_permissions_allow_list() {
    let perms = ToolPermissions {
        allow: vec!["read_file".into(), "list_dir".into()],
        deny: vec![],
    };
    assert!(perms.is_allowed("read_file"));
    assert!(perms.is_allowed("list_dir"));
    assert!(!perms.is_allowed("exec"));
    assert!(!perms.is_allowed("write_file"));
    assert!(!perms.is_allowed("task"));
}

#[test]
fn test_tool_permissions_deny_list() {
    let perms = ToolPermissions {
        allow: vec![],
        deny: vec!["exec".into(), "write_file".into()],
    };
    assert!(perms.is_allowed("read_file"));
    assert!(!perms.is_allowed("exec"));
    assert!(!perms.is_allowed("write_file"));
    assert!(!perms.is_allowed("task"));
}

#[test]
fn test_tool_permissions_allow_and_deny() {
    let perms = ToolPermissions {
        allow: vec!["read_file".into(), "exec".into()],
        deny: vec!["exec".into()],
    };
    assert!(perms.is_allowed("read_file"));
    assert!(!perms.is_allowed("exec")); // deny overrides allow
}

#[test]
fn test_render_agents_catalog_empty() {
    let agents: Vec<SubAgentSpec> = vec![];
    assert!(render_agents_catalog(&agents).is_none());
}

#[test]
fn test_render_agents_catalog_with_agents() {
    let agents = vec![
        SubAgentSpec {
            name: "explore".into(),
            description: "Code exploration".into(),
            system_prompt: String::new(),
            max_turns: 10,
            tools: ToolPermissions::default(),
            mcp_policy: None,
            source: AgentSource::System,
            path: String::new(),
        },
        SubAgentSpec {
            name: "coder".into(),
            description: String::new(),
            system_prompt: String::new(),
            max_turns: 15,
            tools: ToolPermissions::default(),
            mcp_policy: None,
            source: AgentSource::Global,
            path: String::new(),
        },
    ];
    let catalog = render_agents_catalog(&agents).unwrap();
    assert!(catalog.contains("## Sub-Agents"));
    assert!(catalog.contains("**explore**"));
    assert!(catalog.contains("**coder**"));
    assert!(catalog.contains("[`system`]"));
    assert!(catalog.contains("[`global`]"));
    assert!(catalog.contains("Code exploration"));
}

#[test]
fn test_augment_subagent_prompt_with_runtime_context_prepends_local_time() {
    let prompt = "Review the current diff and explain the bug.";
    let augmented = crate::subagents::executor::augment_subagent_prompt_with_runtime_context(
        prompt,
        "2026-04-27 09:30:00 +08:00",
    );

    assert!(augmented.contains("## Delegated Task Context"));
    assert!(augmented.contains("Current system local time: 2026-04-27 09:30:00 +08:00"));
    assert!(augmented.ends_with(prompt));
}

#[test]
fn test_augment_subagent_prompt_with_runtime_context_is_idempotent() {
    let prompt = "## Delegated Task Context\n- Current system local time: 2026-04-27 09:30:00 +08:00\n\n## Delegated Task\nInspect the logs.";
    let augmented = crate::subagents::executor::augment_subagent_prompt_with_runtime_context(
        prompt,
        "2026-04-28 10:45:00 +08:00",
    );

    assert_eq!(augmented, prompt);
}

#[test]
fn test_parse_agent_frontmatter() {
    let content = r#"---
name: test-agent
description: "A test agent"
model: openai/gpt-4o-mini
max_turns: 5
tools:
  allow: [read_file, list_dir]
  deny: [exec]
---

You are a test agent.
"#;
    let spec = crate::subagents::discovery::parse_agent_frontmatter_for_test(content);
    assert!(spec.is_some());
    let spec = spec.unwrap();
    assert_eq!(spec.name, "test-agent");
    assert_eq!(spec.description, "A test agent");
    assert_eq!(spec.max_turns, 5);
    assert_eq!(spec.tools.allow, vec!["read_file", "list_dir"]);
    assert_eq!(spec.tools.deny, vec!["exec"]);
    assert!(spec.system_prompt.contains("You are a test agent."));
}

#[test]
fn test_parse_agent_frontmatter_ignores_legacy_model_field() {
    let content = r#"---
name: legacy-agent
model: anthropic/claude-sonnet-4-20250514
max_turns: 7
---

Legacy agent.
"#;
    let spec = crate::subagents::discovery::parse_agent_frontmatter_for_test(content)
        .expect("legacy AGENT.md should still parse");
    assert_eq!(spec.name, "legacy-agent");
    assert_eq!(spec.max_turns, 7);
    assert!(spec.system_prompt.contains("Legacy agent."));
}

#[test]
fn test_parse_agent_frontmatter_minimal() {
    let content = r#"---
name: minimal
---

Minimal agent.
"#;
    let spec = crate::subagents::discovery::parse_agent_frontmatter_for_test(content);
    assert!(spec.is_some());
    let spec = spec.unwrap();
    assert_eq!(spec.name, "minimal");
    assert!(spec.description.is_empty());
    assert_eq!(spec.max_turns, 15); // default
    assert!(spec.tools.allow.is_empty());
    assert!(spec.tools.deny.is_empty());
}

#[test]
fn test_parse_agent_frontmatter_no_frontmatter() {
    let content = "Just some text without frontmatter.";
    let spec = crate::subagents::discovery::parse_agent_frontmatter_for_test(content);
    assert!(spec.is_none());
}

#[test]
fn test_parse_agent_frontmatter_no_name() {
    let content = r#"---
description: "No name"
---

Body.
"#;
    let spec = crate::subagents::discovery::parse_agent_frontmatter_for_test(content);
    assert!(spec.is_none()); // name is required
}

#[test]
fn test_discover_agents_empty_dir() {
    let temp = std::env::temp_dir().join("lingclaw_test_agents_empty");
    let _ = std::fs::create_dir_all(&temp);
    let agents = discover_all_agents(&temp);
    // May find system agents depending on environment, but should not panic
    let _ = agents;
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_filter_tools_for_agent() {
    let spec = SubAgentSpec {
        name: "test".into(),
        description: String::new(),
        system_prompt: String::new(),
        max_turns: 10,
        tools: ToolPermissions {
            allow: vec!["read_file".into(), "list_dir".into()],
            deny: vec![],
        },
        mcp_policy: None,
        source: AgentSource::System,
        path: String::new(),
    };
    let tools = crate::subagents::filter_tools_for_agent(&spec);
    assert!(tools.contains(&"read_file".to_string()));
    assert!(tools.contains(&"list_dir".to_string()));
    assert!(!tools.contains(&"exec".to_string()));
    assert!(!tools.contains(&"task".to_string()));
}

#[test]
fn test_filter_tools_for_agent_with_mcp_no_servers() {
    // When no MCP servers are configured, with_mcp yields the same result as the built-in filter.
    let spec = SubAgentSpec {
        name: "test".into(),
        description: String::new(),
        system_prompt: String::new(),
        max_turns: 10,
        tools: ToolPermissions {
            allow: vec!["read_file".into(), "list_dir".into()],
            deny: vec![],
        },
        mcp_policy: None,
        source: AgentSource::System,
        path: String::new(),
    };
    let config = base_config();
    let workspace = std::env::temp_dir();
    let tools = crate::subagents::filter_tools_for_agent_with_mcp(&spec, &config, &workspace);
    assert!(tools.contains(&"read_file".to_string()));
    assert!(tools.contains(&"list_dir".to_string()));
    assert!(!tools.contains(&"exec".to_string()));
    assert!(!tools.contains(&"task".to_string()));

    // Should match built-in-only filter when no MCP tools exist.
    let builtin_only = crate::subagents::filter_tools_for_agent(&spec);
    assert_eq!(tools, builtin_only);
}

// --- MCP policy and tool classification tests ---

use crate::subagents::{McpPolicy, is_mcp_tool_read_only};
use crate::tools::mcp::McpToolDescriptor;

fn make_mcp_descriptor(raw_name: &str, description: &str) -> McpToolDescriptor {
    McpToolDescriptor {
        server_name: "test-server".into(),
        raw_name: raw_name.into(),
        exposed_name: format!("mcp__test_server__{raw_name}"),
        description: description.into(),
        input_schema: serde_json::json!({}),
    }
}

#[test]
fn test_mcp_tool_classification_read_only() {
    // Tools with read-only names should be classified as read-only.
    assert!(is_mcp_tool_read_only(&make_mcp_descriptor(
        "get_file_contents",
        "Retrieve the contents of a file"
    )));
    assert!(is_mcp_tool_read_only(&make_mcp_descriptor(
        "list_repos",
        "List available repositories"
    )));
    assert!(is_mcp_tool_read_only(&make_mcp_descriptor(
        "search_code",
        "Search for code patterns"
    )));
    assert!(is_mcp_tool_read_only(&make_mcp_descriptor(
        "describe_table",
        "Show table schema information"
    )));
}

#[test]
fn test_mcp_tool_classification_mutating() {
    // Tools with mutation keywords in name should NOT be classified as read-only.
    assert!(!is_mcp_tool_read_only(&make_mcp_descriptor(
        "create_issue",
        "Create a new GitHub issue"
    )));
    assert!(!is_mcp_tool_read_only(&make_mcp_descriptor(
        "delete_branch",
        "Delete a git branch"
    )));
    assert!(!is_mcp_tool_read_only(&make_mcp_descriptor(
        "update_pull_request",
        "Update a pull request"
    )));
    assert!(!is_mcp_tool_read_only(&make_mcp_descriptor(
        "execute_query",
        "Execute a SQL query"
    )));
    assert!(!is_mcp_tool_read_only(&make_mcp_descriptor(
        "send_message",
        "Send a chat message"
    )));
}

#[test]
fn test_mcp_tool_classification_description_only_mutation() {
    // Tool with innocent name but mutation keyword in description.
    assert!(!is_mcp_tool_read_only(&make_mcp_descriptor(
        "query",
        "Execute and modify database records"
    )));
    assert!(!is_mcp_tool_read_only(&make_mcp_descriptor(
        "manage_workflow",
        "Start or stop CI workflows"
    )));
}

#[test]
fn test_mcp_tool_classification_no_false_positives_from_substrings() {
    // Words like "offset", "settings", "address" contain short keywords
    // but should NOT trigger a mutation classification.
    assert!(is_mcp_tool_read_only(&make_mcp_descriptor(
        "get_offset",
        "Retrieve the current offset"
    )));
    assert!(is_mcp_tool_read_only(&make_mcp_descriptor(
        "read_settings",
        "Load settings from configuration"
    )));
    assert!(is_mcp_tool_read_only(&make_mcp_descriptor(
        "lookup_address",
        "Look up an address record"
    )));
}

#[test]
fn test_mcp_tool_classification_defaults_unknown_tools_to_mutating() {
    assert!(!is_mcp_tool_read_only(&make_mcp_descriptor(
        "clone_repo",
        "Repository operation"
    )));
    assert!(!is_mcp_tool_read_only(&make_mcp_descriptor(
        "operation_42",
        "Tool exposed by MCP server"
    )));
}

#[tokio::test]
async fn test_mcp_read_only_tools_are_parallelizable_for_dispatch() {
    let workspace = unique_temp_workspace("lingclaw-subagent-parallel-readonly-mcp");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let log_path = workspace.join("mock.log");

    let config = base_config_with_mock_mcp_server("normal", &log_path);
    crate::tools::mcp::ensure_tools_cached(&config, &workspace).await;
    let reports = crate::tools::mcp::refresh_servers(&config, &workspace)
        .await
        .expect("mock MCP server should refresh");
    assert_eq!(reports.len(), 1);
    assert!(reports[0].error.is_none(), "{:?}", reports[0].error);
    let tool_name = reports[0]
        .tool_names
        .first()
        .cloned()
        .expect("mock MCP server should expose a tool");

    let descriptor = crate::tools::mcp::cached_list_tools(&config, &workspace)
        .into_iter()
        .find(|tool| tool.exposed_name == tool_name)
        .expect("mock MCP tool should be cached");

    assert!(is_mcp_tool_read_only(&descriptor));
    assert!(crate::tools::is_read_only_tool("read_file"));
    assert!(!crate::tools::is_read_only_tool(&tool_name));
    assert!(crate::tools::is_parallelizable_tool_call(
        &tool_name, &config, &workspace
    ));
    assert!(!crate::tools::is_read_only_tool("exec"));

    let _ = crate::tools::mcp::refresh_servers(&config, &workspace).await;
    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn test_parse_agent_frontmatter_with_mcp_policy() {
    let content = r#"---
name: smart-reader
description: "A smart reader agent"
mcp_policy: read_only
tools:
  allow: [read_file, list_dir]
  deny: []
---

Smart reader.
"#;
    let spec = crate::subagents::discovery::parse_agent_frontmatter_for_test(content)
        .expect("should parse");
    assert_eq!(spec.name, "smart-reader");
    assert_eq!(spec.mcp_policy, Some(McpPolicy::ReadOnly));
}

#[test]
fn test_parse_agent_frontmatter_mcp_policy_all() {
    let content = r#"---
name: power-agent
mcp_policy: all
---

Power agent.
"#;
    let spec = crate::subagents::discovery::parse_agent_frontmatter_for_test(content)
        .expect("should parse");
    assert_eq!(spec.mcp_policy, Some(McpPolicy::All));
}

#[test]
fn test_parse_agent_frontmatter_no_mcp_policy() {
    let content = r#"---
name: classic-agent
tools:
  allow: [read_file]
  deny: []
---

Classic agent.
"#;
    let spec = crate::subagents::discovery::parse_agent_frontmatter_for_test(content)
        .expect("should parse");
    assert_eq!(spec.mcp_policy, None);
}

#[test]
fn test_parse_agent_frontmatter_invalid_mcp_policy() {
    let content = r#"---
name: bad-policy
mcp_policy: something_invalid
---

Bad policy.
"#;
    let spec = crate::subagents::discovery::parse_agent_frontmatter_for_test(content)
        .expect("should parse");
    assert_eq!(spec.mcp_policy, None); // unknown values are ignored
}

// --- Sub-agent model resolution tests ---
// These tests call the production `resolve_subagent_model()` function.

use crate::Config;
use crate::config::{JsonMcpServerConfig, JsonModelEntry, JsonProviderConfig, Provider};
use crate::hooks::{AgentHook, HookInput, HookOutput, HookRegistry, ToolHookInput};
use crate::subagents::executor::resolve_subagent_model;
use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command as StdCommand;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

const MOCK_MCP_SERVER_SOURCE: &str = include_str!("fixtures/mock_mcp_server.rs");

struct CapturedHttpRequest {
    body: String,
}

fn base_config() -> Config {
    Config {
        api_key: String::new(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "openai/gpt-4o".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: Provider::OpenAI,
        anthropic_prompt_caching: false,
        openai_stream_include_usage: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: 18989,
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

fn unique_temp_workspace(prefix: &str) -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time should be after unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{unique}"))
}

fn mock_server_binary() -> &'static PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let helper_dir = std::env::temp_dir().join("lingclaw-subagent-mcp-test-helper");
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

fn base_config_with_mock_mcp_server(mode: &str, log_path: &Path) -> Config {
    let mut config = base_config();
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

fn log_line_count(log_path: &Path, needle: &str) -> usize {
    fs::read_to_string(log_path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains(needle))
        .count()
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(header_text: &str) -> usize {
    header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn read_http_request(stream: &mut TcpStream) -> CapturedHttpRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout should be set");

    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];

    loop {
        let read = stream.read(&mut chunk).expect("request should be readable");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);

        if let Some(headers_end) = find_headers_end(&buffer) {
            let header_text = String::from_utf8_lossy(&buffer[..headers_end + 4]);
            let content_length = parse_content_length(&header_text);
            let total_len = headers_end + 4 + content_length;
            if buffer.len() >= total_len {
                break;
            }
        }
    }

    let headers_end = find_headers_end(&buffer).expect("request should contain headers");
    let body = String::from_utf8_lossy(&buffer[headers_end + 4..]).to_string();
    CapturedHttpRequest { body }
}

fn spawn_one_shot_http_server(
    response_content_type: &'static str,
    response_body: String,
) -> (String, thread::JoinHandle<()>) {
    spawn_http_server_with_responses(vec![(response_content_type, response_body)])
}

fn spawn_http_server_with_responses(
    responses: Vec<(&'static str, String)>,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should expose address");

    let handle = thread::spawn(move || {
        for (response_content_type, response_body) in responses {
            let (mut stream, _) = listener.accept().expect("request should connect");
            let _ = read_http_request(&mut stream);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {response_content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream
                .write_all(response.as_bytes())
                .expect("response should be written");
            stream.flush().expect("response should flush");
        }
    });

    (format!("http://{}", address), handle)
}

fn spawn_one_shot_http_server_with_capture(
    response_content_type: &'static str,
    response_body: String,
) -> (
    String,
    std::sync::mpsc::Receiver<CapturedHttpRequest>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should expose address");
    let (request_tx, request_rx) = std::sync::mpsc::channel();

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("request should connect");
        let request = read_http_request(&mut stream);
        request_tx
            .send(request)
            .expect("captured request should be sent");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {response_content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("response should be written");
        stream.flush().expect("response should flush");
    });

    (format!("http://{}", address), request_rx, handle)
}

fn build_openai_tool_call_stream(tool_name: &str, args: serde_json::Value) -> String {
    let args_json = serde_json::to_string(&args).expect("tool args should serialize");
    let chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {
                        "name": tool_name,
                        "arguments": args_json
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    format!("data: {}\n\ndata: [DONE]\n\n", chunk)
}

fn build_openai_multi_tool_call_stream(tool_calls: Vec<(&str, serde_json::Value)>) -> String {
    let calls: Vec<_> = tool_calls
        .into_iter()
        .enumerate()
        .map(|(index, (tool_name, args))| {
            let args_json = serde_json::to_string(&args).expect("tool args should serialize");
            serde_json::json!({
                "index": index,
                "id": format!("call_{}", index + 1),
                "function": {
                    "name": tool_name,
                    "arguments": args_json
                }
            })
        })
        .collect();
    let chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": calls
            },
            "finish_reason": "tool_calls"
        }]
    });
    format!("data: {}\n\ndata: [DONE]\n\n", chunk)
}

fn build_openai_content_stream(content: &str) -> String {
    let chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "content": content
            }
        }]
    });
    format!("data: {}\n\ndata: [DONE]\n\n", chunk)
}

fn build_openai_content_then_tool_call_stream(
    content: &str,
    tool_name: &str,
    args: serde_json::Value,
) -> String {
    let args_json = serde_json::to_string(&args).expect("tool args should serialize");
    let content_chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "content": content
            }
        }]
    });
    let tool_chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {
                        "name": tool_name,
                        "arguments": args_json
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    });
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        content_chunk, tool_chunk
    )
}

fn build_openai_simple_text_response(content: &str) -> String {
    serde_json::json!({
        "choices": [{
            "message": {
                "content": content
            }
        }],
        "usage": {
            "prompt_tokens": 120,
            "completion_tokens": 32
        }
    })
    .to_string()
}

fn slow_tool_command() -> String {
    if cfg!(windows) {
        "ping -n 3 127.0.0.1 > NUL".to_string()
    } else {
        "while :; do :; done".to_string()
    }
}

#[tokio::test]
async fn collect_parallel_tool_results_preserves_finished_results_on_deadline() {
    let fast_future = Box::pin(async {
        Some(crate::tools::ToolOutcome {
            output: "fast result".to_string(),
            is_error: false,
            duration_ms: 1,
            subagent_snapshot: None,
        })
    })
        as std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Option<crate::tools::ToolOutcome>>
                    + Send
                    + 'static,
            >,
        >;
    let slow_future = Box::pin(async {
        std::future::pending::<()>().await;
        #[allow(unreachable_code)]
        None
    })
        as std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Option<crate::tools::ToolOutcome>>
                    + Send
                    + 'static,
            >,
        >;

    let cancel = CancellationToken::new();
    let batch = crate::subagents::executor::collect_parallel_tool_results(
        vec![fast_future, slow_future],
        &cancel,
        Some(tokio::time::Instant::now() + Duration::from_millis(20)),
    )
    .await;

    assert!(batch.interrupted);
    assert!(batch.timed_out);
    assert_eq!(batch.results.len(), 2);
    assert_eq!(
        batch.results[0].as_ref().map(|o| o.output.as_str()),
        Some("fast result")
    );
    assert!(batch.results[1].is_none());
}

/// Sub-agents use the configured delegated model when set.
#[test]
fn test_model_resolution_prefers_sub_agent_config() {
    let config = Config {
        sub_agent_model: Some("openai/gpt-4o-mini".to_string()),
        ..base_config()
    };
    assert_eq!(
        resolve_subagent_model(&config, "reviewer"),
        "openai/gpt-4o-mini"
    );
}

/// Falls back to config.model when no dedicated sub-agent model is configured.
#[test]
fn test_model_resolution_falls_back_to_primary() {
    let config = base_config(); // sub_agent_model = None
    assert_eq!(resolve_subagent_model(&config, "reviewer"), "openai/gpt-4o");
}

#[test]
fn test_model_resolution_prefers_specific_sub_agent_override() {
    let mut config = base_config();
    config.sub_agent_model = Some("openai/gpt-4o-mini".to_string());
    config.sub_agent_model_overrides.insert(
        "reviewer".to_string(),
        "anthropic/claude-sonnet-4-20250514".to_string(),
    );
    assert_eq!(
        resolve_subagent_model(&config, "reviewer"),
        "anthropic/claude-sonnet-4-20250514"
    );
    assert_eq!(
        resolve_subagent_model(&config, "coder"),
        "openai/gpt-4o-mini"
    );
}

/// max_turns is clamped to MAX_AGENT_TURNS even when AGENT.md specifies a higher value.
#[test]
fn test_max_turns_clamped_to_hard_limit() {
    let content = r#"---
name: excessive
max_turns: 999
---

Runaway agent.
"#;
    let spec = crate::subagents::discovery::parse_agent_frontmatter_for_test(content)
        .expect("should parse");
    assert_eq!(spec.max_turns, crate::subagents::MAX_AGENT_TURNS);
}

/// max_turns below the cap is preserved.
#[test]
fn test_max_turns_within_cap_preserved() {
    let content = r#"---
name: normal
max_turns: 20
---

Normal agent.
"#;
    let spec = crate::subagents::discovery::parse_agent_frontmatter_for_test(content)
        .expect("should parse");
    assert_eq!(spec.max_turns, 20);
}

struct TimeoutMarkerHook;

struct ObservedTimeoutHook {
    expected_command: String,
    called: Arc<AtomicBool>,
}

struct RecordingAfterToolHook {
    seen_tools: Arc<std::sync::Mutex<Vec<String>>>,
}

struct GuardedAfterToolHook {
    called: Arc<AtomicBool>,
}

impl AgentHook for TimeoutMarkerHook {
    fn name(&self) -> &'static str {
        "timeout_marker"
    }

    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::AfterToolExec
    }

    fn should_run(&self, _: &[ChatMessage], _: Provider, _: usize, _: usize) -> bool {
        false
    }

    fn run<'a>(
        &'a self,
        _: HookInput,
        _: &'a Config,
        _: &'a reqwest::Client,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async { HookOutput::NoOp })
    }

    fn should_run_tool(&self, tool_name: &str, point: agent::HookPoint) -> bool {
        point == agent::HookPoint::AfterToolExec && tool_name == "exec"
    }

    fn run_tool<'a>(
        &'a self,
        input: ToolHookInput,
        _: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async move {
            assert_eq!(input.outcome_is_error, Some(true));
            assert_eq!(input.outcome_duration_ms, Some(12));
            assert_eq!(input.tool_args["command"], "sleep 5");
            HookOutput::ModifyToolResult {
                result: format!(
                    "{} [after-hook]",
                    input
                        .outcome_output
                        .expect("timeout output should be present")
                ),
            }
        })
    }
}

impl AgentHook for ObservedTimeoutHook {
    fn name(&self) -> &'static str {
        "observed_timeout"
    }

    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::AfterToolExec
    }

    fn should_run(&self, _: &[ChatMessage], _: Provider, _: usize, _: usize) -> bool {
        false
    }

    fn run<'a>(
        &'a self,
        _: HookInput,
        _: &'a Config,
        _: &'a reqwest::Client,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async { HookOutput::NoOp })
    }

    fn should_run_tool(&self, tool_name: &str, point: agent::HookPoint) -> bool {
        point == agent::HookPoint::AfterToolExec && tool_name == "exec"
    }

    fn run_tool<'a>(
        &'a self,
        input: ToolHookInput,
        _: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async move {
            self.called.store(true, Ordering::SeqCst);
            assert_eq!(input.outcome_is_error, Some(true));
            assert_eq!(
                input.tool_args["command"].as_str(),
                Some(self.expected_command.as_str())
            );
            assert!(input.outcome_duration_ms.is_some());
            assert!(
                input
                    .outcome_output
                    .as_deref()
                    .is_some_and(|text| text.contains("deadline exceeded"))
            );
            HookOutput::NoOp
        })
    }
}

impl AgentHook for RecordingAfterToolHook {
    fn name(&self) -> &'static str {
        "recording_after_tool"
    }

    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::AfterToolExec
    }

    fn should_run(&self, _: &[ChatMessage], _: Provider, _: usize, _: usize) -> bool {
        false
    }

    fn run<'a>(
        &'a self,
        _: HookInput,
        _: &'a Config,
        _: &'a reqwest::Client,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async { HookOutput::NoOp })
    }

    fn should_run_tool(&self, _: &str, point: agent::HookPoint) -> bool {
        point == agent::HookPoint::AfterToolExec
    }

    fn run_tool<'a>(
        &'a self,
        input: ToolHookInput,
        _: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async move {
            self.seen_tools
                .lock()
                .expect("after-tool hook state should lock")
                .push(input.tool_name.clone());
            HookOutput::NoOp
        })
    }
}

impl AgentHook for GuardedAfterToolHook {
    fn name(&self) -> &'static str {
        "guarded_after_tool"
    }

    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::AfterToolExec
    }

    fn should_run(&self, _: &[ChatMessage], _: Provider, _: usize, _: usize) -> bool {
        false
    }

    fn run<'a>(
        &'a self,
        _: HookInput,
        _: &'a Config,
        _: &'a reqwest::Client,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async { HookOutput::NoOp })
    }

    fn should_run_tool(&self, _: &str, point: agent::HookPoint) -> bool {
        point == agent::HookPoint::AfterToolExec
    }

    fn run_tool<'a>(
        &'a self,
        _: ToolHookInput,
        _: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async move {
            self.called.store(true, Ordering::SeqCst);
            HookOutput::NoOp
        })
    }
}

#[tokio::test]
async fn timeout_outcome_still_runs_after_tool_exec_hook() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(TimeoutMarkerHook));

    let workspace = std::env::temp_dir();
    let outcome = crate::subagents::executor::apply_after_tool_exec_hook(
        &registry,
        &base_config(),
        &workspace,
        1,
        "exec",
        r#"{"command":"sleep 5"}"#,
        "tc-timeout",
        crate::tools::ToolOutcome {
            output: "Tool 'exec' aborted: sub-agent deadline exceeded".to_string(),
            is_error: true,
            duration_ms: 12,
            subagent_snapshot: None,
        },
    )
    .await;

    assert!(outcome.is_error);
    assert_eq!(outcome.duration_ms, 12);
    assert_eq!(
        outcome.output,
        "Tool 'exec' aborted: sub-agent deadline exceeded [after-hook]"
    );
}

#[tokio::test]
async fn interrupted_parallel_batch_outcome_skips_after_tool_exec_hook_without_result() {
    let called = Arc::new(AtomicBool::new(false));
    let mut registry = HookRegistry::new();
    registry.register(Box::new(GuardedAfterToolHook {
        called: called.clone(),
    }));

    let workspace = std::env::temp_dir();
    let outcome = crate::subagents::executor::finalize_parallel_batch_outcome(
        &registry,
        &base_config(),
        &workspace,
        1,
        "read_file",
        r#"{"path":"notes.txt"}"#,
        "tc-interrupted",
        None,
        true,
        true,
        25,
    )
    .await;

    assert!(!called.load(Ordering::SeqCst));
    assert!(outcome.is_error);
    assert_eq!(outcome.duration_ms, 25);
    assert!(outcome.output.contains("deadline exceeded"));
}

#[tokio::test]
async fn run_subagent_multi_read_only_batch_executes_after_tool_exec_hooks() {
    let workspace = unique_temp_workspace("lingclaw-subagent-readonly-batch");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::write(workspace.join("notes.txt"), "alpha\nbeta\n")
        .expect("fixture file should be written");

    let response_body = build_openai_multi_tool_call_stream(vec![
        (
            "read_file",
            serde_json::json!({
                "path": "notes.txt"
            }),
        ),
        (
            "list_dir",
            serde_json::json!({
                "path": "."
            }),
        ),
    ]);
    let (api_base, handle) = spawn_one_shot_http_server("text/event-stream", response_body);

    let mut config = base_config();
    config.api_base = api_base;
    config.api_key = "test-key".to_string();

    let spec = SubAgentSpec {
        name: "batch-agent".into(),
        description: String::new(),
        system_prompt: "Inspect the workspace with read-only tools.".into(),
        max_turns: 1,
        tools: ToolPermissions {
            allow: vec!["read_file".into(), "list_dir".into()],
            deny: vec![],
        },
        mcp_policy: None,
        source: AgentSource::System,
        path: String::new(),
    };

    let seen_tools = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Box::new(RecordingAfterToolHook {
        seen_tools: seen_tools.clone(),
    }));

    let (live_tx, _live_rx) = tokio::sync::mpsc::channel(16);
    let http = reqwest::Client::new();
    let outcome = crate::subagents::executor::run_subagent(
        &spec,
        "Read the file and list the directory.",
        &config,
        &http,
        &workspace,
        &live_tx,
        tokio_util::sync::CancellationToken::new(),
        &hooks,
        "test-task-1",
    )
    .await;

    handle.join().expect("server thread should join");

    let seen = seen_tools
        .lock()
        .expect("after-tool hook state should lock")
        .clone();
    assert!(!outcome.aborted);
    assert_eq!(outcome.cycles, 1);
    assert_eq!(outcome.tool_calls, 2);
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().any(|tool| tool == "read_file"));
    assert!(seen.iter().any(|tool| tool == "list_dir"));

    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn run_subagent_emits_tool_result_event_for_completed_tool() {
    let workspace = unique_temp_workspace("lingclaw-subagent-tool-result-event");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::write(workspace.join("notes.txt"), "alpha\nbeta\n")
        .expect("fixture file should be written");

    let response_body = build_openai_tool_call_stream(
        "read_file",
        serde_json::json!({
            "path": "notes.txt"
        }),
    );
    let (api_base, handle) = spawn_one_shot_http_server("text/event-stream", response_body);

    let mut config = base_config();
    config.api_base = api_base;
    config.api_key = "test-key".to_string();

    let spec = SubAgentSpec {
        name: "tool-result-agent".into(),
        description: String::new(),
        system_prompt: "Read the delegated file.".into(),
        max_turns: 1,
        tools: ToolPermissions {
            allow: vec!["read_file".into()],
            deny: vec![],
        },
        mcp_policy: None,
        source: AgentSource::System,
        path: String::new(),
    };

    let (live_tx, mut live_rx) = tokio::sync::mpsc::channel(16);
    let http = reqwest::Client::new();
    let outcome = crate::subagents::executor::run_subagent(
        &spec,
        "Read notes.txt.",
        &config,
        &http,
        &workspace,
        &live_tx,
        tokio_util::sync::CancellationToken::new(),
        &HookRegistry::new(),
        "test-task-tool-result",
    )
    .await;

    handle.join().expect("server thread should join");

    assert!(!outcome.aborted);
    assert_eq!(outcome.cycles, 1);
    assert_eq!(outcome.tool_calls, 1);

    let mut saw_task_tool = false;
    let mut saw_tool_result = false;
    while let Ok(event) = live_rx.try_recv() {
        match event["type"].as_str() {
            Some("task_tool") => {
                saw_task_tool = true;
                assert_eq!(event["task_id"].as_str(), Some("test-task-tool-result"));
                assert_eq!(event["agent"].as_str(), Some("tool-result-agent"));
                assert_eq!(event["tool"].as_str(), Some("read_file"));
                assert_eq!(event["id"].as_str(), Some("call_1"));
                assert_eq!(
                    event["arguments"].as_str(),
                    Some("{\"path\":\"notes.txt\"}")
                );
            }
            Some("tool_result") => {
                saw_tool_result = true;
                assert_eq!(event["task_id"].as_str(), Some("test-task-tool-result"));
                assert_eq!(event["subagent"].as_str(), Some("tool-result-agent"));
                assert_eq!(event["name"].as_str(), Some("read_file"));
                assert_eq!(event["id"].as_str(), Some("call_1"));
                assert_eq!(event["is_error"].as_bool(), Some(false));
                assert!(
                    event["result"]
                        .as_str()
                        .is_some_and(|result| result.contains("alpha"))
                );
                assert!(event["duration_ms"].as_u64().is_some());
            }
            _ => {}
        }
    }

    assert!(saw_task_tool);
    assert!(saw_tool_result);

    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn run_subagent_forces_final_summary_after_tool_only_last_turn() {
    let workspace = unique_temp_workspace("lingclaw-subagent-forced-final-summary");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::write(workspace.join("notes.txt"), "alpha\nbeta\n")
        .expect("fixture file should be written");

    let stream_body = build_openai_content_then_tool_call_stream(
        "Now let me check a few more things to verify specific issues: ",
        "read_file",
        serde_json::json!({
            "path": "notes.txt"
        }),
    );
    let summary_body = build_openai_simple_text_response(
        "Findings:\n- Reviewed notes.txt\n- Ready to hand the final summary back to the parent agent.",
    );
    let (api_base, handle) = spawn_http_server_with_responses(vec![
        ("text/event-stream", stream_body),
        ("application/json", summary_body),
    ]);

    let mut config = base_config();
    config.api_base = api_base;
    config.api_key = "test-key".to_string();

    let spec = SubAgentSpec {
        name: "forced-summary-agent".into(),
        description: String::new(),
        system_prompt: "Inspect the delegated file and return a final review.".into(),
        max_turns: 1,
        tools: ToolPermissions {
            allow: vec!["read_file".into()],
            deny: vec![],
        },
        mcp_policy: None,
        source: AgentSource::System,
        path: String::new(),
    };

    let (live_tx, _live_rx) = tokio::sync::mpsc::channel(16);
    let http = reqwest::Client::new();
    let outcome = crate::subagents::executor::run_subagent(
        &spec,
        "Review notes.txt.",
        &config,
        &http,
        &workspace,
        &live_tx,
        tokio_util::sync::CancellationToken::new(),
        &HookRegistry::new(),
        "test-task-forced-summary",
    )
    .await;

    handle.join().expect("server thread should join");

    assert!(!outcome.aborted);
    assert_eq!(outcome.cycles, 1);
    assert_eq!(outcome.tool_calls, 1);
    assert!(outcome.result.contains("Findings:"));
    assert!(!outcome.result.contains("Now let me check"));
    assert!(outcome.total_input_tokens >= 120);
    assert!(outcome.total_output_tokens >= 32);

    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn run_subagent_configured_openai_gateway_auto_disables_reasoning_controls() {
    let workspace = unique_temp_workspace("lingclaw-subagent-configured-openai-auto-think");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");

    let response_body = build_openai_content_stream("Frontend task complete.");
    let (api_base, request_rx, handle) =
        spawn_one_shot_http_server_with_capture("text/event-stream", response_body);

    let mut config = base_config();
    config.providers.insert(
        "gateway".to_string(),
        JsonProviderConfig {
            base_url: api_base,
            api_key: "test-key".to_string(),
            api: "openai-completions".to_string(),
            models: vec![JsonModelEntry {
                id: "kimi-k2.6".to_string(),
                name: None,
                reasoning: Some(true),
                input: Some(vec!["text".to_string(), "image".to_string()]),
                cost: None,
                context_window: Some(256_000),
                max_tokens: Some(32_000),
                compat: None,
            }],
        },
    );
    config.sub_agent_model_overrides.insert(
        "frontend-coder".to_string(),
        "gateway/kimi-k2.6".to_string(),
    );

    let spec = SubAgentSpec {
        name: "frontend-coder".into(),
        description: String::new(),
        system_prompt: "Implement the requested UI update.".into(),
        max_turns: 1,
        tools: ToolPermissions::default(),
        mcp_policy: None,
        source: AgentSource::System,
        path: String::new(),
    };

    let (live_tx, _live_rx) = tokio::sync::mpsc::channel(16);
    let http = reqwest::Client::new();
    let outcome = crate::subagents::executor::run_subagent(
        &spec,
        "Center the modal and polish spacing.",
        &config,
        &http,
        &workspace,
        &live_tx,
        tokio_util::sync::CancellationToken::new(),
        &HookRegistry::new(),
        "test-task-configured-openai-auto-think",
    )
    .await;

    let request = request_rx.recv().expect("request should be captured");
    handle.join().expect("server thread should join");

    let body: serde_json::Value =
        serde_json::from_str(&request.body).expect("request body should be valid json");
    let tool_names: Vec<&str> = body["tools"]
        .as_array()
        .expect("tool definitions should be an array")
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect();
    let unique_tool_names: std::collections::HashSet<&str> = tool_names.iter().copied().collect();

    assert!(!outcome.aborted);
    assert_eq!(outcome.result, "Frontend task complete.");
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("enable_thinking").is_none());
    assert_eq!(
        tool_names.len(),
        unique_tool_names.len(),
        "sub-agent requests should not duplicate tool definitions"
    );

    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn run_subagent_sequential_tools_emit_interleaved_tool_events() {
    let workspace = unique_temp_workspace("lingclaw-subagent-sequential-tool-events");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    fs::write(workspace.join("notes.txt"), "alpha\nbeta\n")
        .expect("fixture file should be written");

    let response_body = build_openai_multi_tool_call_stream(vec![
        (
            "read_file",
            serde_json::json!({
                "path": "notes.txt"
            }),
        ),
        (
            "exec",
            serde_json::json!({
                "command": "echo sequential"
            }),
        ),
    ]);
    let (api_base, handle) = spawn_one_shot_http_server("text/event-stream", response_body);

    let mut config = base_config();
    config.api_base = api_base;
    config.api_key = "test-key".to_string();

    let spec = SubAgentSpec {
        name: "sequential-event-agent".into(),
        description: String::new(),
        system_prompt: "Read the file and run a harmless command.".into(),
        max_turns: 1,
        tools: ToolPermissions {
            allow: vec!["read_file".into(), "exec".into()],
            deny: vec![],
        },
        mcp_policy: None,
        source: AgentSource::System,
        path: String::new(),
    };

    let (live_tx, mut live_rx) = tokio::sync::mpsc::channel(16);
    let http = reqwest::Client::new();
    let outcome = crate::subagents::executor::run_subagent(
        &spec,
        "Read notes.txt and echo sequential.",
        &config,
        &http,
        &workspace,
        &live_tx,
        tokio_util::sync::CancellationToken::new(),
        &HookRegistry::new(),
        "test-task-sequential-events",
    )
    .await;

    handle.join().expect("server thread should join");

    assert!(!outcome.aborted);
    assert_eq!(outcome.cycles, 1);
    assert_eq!(outcome.tool_calls, 2);

    let mut tool_events = Vec::new();
    while let Ok(event) = live_rx.try_recv() {
        if matches!(event["type"].as_str(), Some("task_tool" | "tool_result")) {
            tool_events.push(event);
        }
    }

    assert_eq!(tool_events.len(), 4);
    assert_eq!(tool_events[0]["type"], "task_tool");
    assert_eq!(tool_events[0]["id"], "call_1");
    assert_eq!(tool_events[0]["tool"], "read_file");
    assert!(
        tool_events[0]["arguments"]
            .as_str()
            .is_some_and(|args| args.contains("notes.txt"))
    );
    assert_eq!(tool_events[1]["type"], "tool_result");
    assert_eq!(tool_events[1]["id"], "call_1");
    assert_eq!(tool_events[1]["name"], "read_file");
    assert!(
        tool_events[1]["result"]
            .as_str()
            .is_some_and(|result| result.contains("alpha"))
    );
    assert_eq!(tool_events[2]["type"], "task_tool");
    assert_eq!(tool_events[2]["id"], "call_2");
    assert_eq!(tool_events[2]["tool"], "exec");
    assert!(
        tool_events[2]["arguments"]
            .as_str()
            .is_some_and(|args| args.contains("echo sequential"))
    );
    assert_eq!(tool_events[3]["type"], "tool_result");
    assert_eq!(tool_events[3]["id"], "call_2");
    assert_eq!(tool_events[3]["name"], "exec");
    assert!(
        tool_events[3]["result"]
            .as_str()
            .is_some_and(|result| result.contains("sequential"))
    );

    let _ = fs::remove_dir_all(&workspace);
}

/// Integration test: chains Phase 3 (`collect_parallel_tool_results` with a deadline
/// that interrupts one tool) into Phase 4 (`finalize_parallel_batch_outcome` for each
/// slot). Verifies that `AfterToolExec` fires only for the tool whose future completed,
/// and the interrupted slot receives a synthetic error without triggering the hook.
#[tokio::test]
async fn parallel_batch_interrupt_fires_hooks_only_for_completed_tools() {
    // Phase 3: One fast future and one pending (never-completes) future.
    let fast_future = Box::pin(async {
        Some(crate::tools::ToolOutcome {
            output: "file contents here".to_string(),
            is_error: false,
            duration_ms: 1,
            subagent_snapshot: None,
        })
    })
        as std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Option<crate::tools::ToolOutcome>>
                    + Send
                    + 'static,
            >,
        >;
    let slow_future = Box::pin(async {
        std::future::pending::<()>().await;
        #[allow(unreachable_code)]
        None
    })
        as std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Option<crate::tools::ToolOutcome>>
                    + Send
                    + 'static,
            >,
        >;

    let cancel = CancellationToken::new();
    let batch = crate::subagents::executor::collect_parallel_tool_results(
        vec![fast_future, slow_future],
        &cancel,
        Some(tokio::time::Instant::now() + Duration::from_millis(20)),
    )
    .await;

    assert!(batch.interrupted);
    assert!(batch.timed_out);
    assert_eq!(batch.results.len(), 2);
    // Fast tool completed.
    assert!(batch.results[0].is_some());
    // Slow tool did not complete.
    assert!(batch.results[1].is_none());

    // Phase 4: Record results through finalize_parallel_batch_outcome.
    let seen_tools = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register(Box::new(RecordingAfterToolHook {
        seen_tools: seen_tools.clone(),
    }));
    let config = base_config();
    let workspace = std::env::temp_dir();
    let elapsed_ms = 20_u64;

    let mut results_iter = batch.results.into_iter();

    // Slot 0: read_file (completed)
    let outcome0 = crate::subagents::executor::finalize_parallel_batch_outcome(
        &hooks,
        &config,
        &workspace,
        1,
        "read_file",
        r#"{"path":"notes.txt"}"#,
        "tc-0",
        results_iter.next().unwrap(),
        batch.interrupted,
        batch.timed_out,
        elapsed_ms,
    )
    .await;

    // Slot 1: list_dir (interrupted — None)
    let outcome1 = crate::subagents::executor::finalize_parallel_batch_outcome(
        &hooks,
        &config,
        &workspace,
        1,
        "list_dir",
        r#"{"path":"."}"#,
        "tc-1",
        results_iter.next().unwrap(),
        batch.interrupted,
        batch.timed_out,
        elapsed_ms,
    )
    .await;

    // AfterToolExec should fire only for the completed tool.
    let seen = seen_tools.lock().expect("hook state should lock").clone();
    assert_eq!(
        seen.len(),
        1,
        "AfterToolExec should fire only for completed tools, got: {seen:?}"
    );
    assert_eq!(seen[0], "read_file");

    // Completed tool gets its real result through the hook pipeline.
    assert!(!outcome0.is_error);
    assert_eq!(outcome0.output, "file contents here");

    // Interrupted tool gets a synthetic error without touching the hook.
    assert!(outcome1.is_error);
    assert!(
        outcome1.output.contains("deadline exceeded"),
        "expected deadline exceeded message, got: {}",
        outcome1.output
    );
}

#[tokio::test]
async fn run_subagent_timeout_during_tool_exec_still_runs_after_tool_exec_hook() {
    let command = slow_tool_command();
    let response_body =
        build_openai_tool_call_stream("exec", serde_json::json!({ "command": command.clone() }));
    let (api_base, handle) = spawn_one_shot_http_server("text/event-stream", response_body);

    let mut config = base_config();
    config.api_base = api_base;
    config.api_key = "test-key".to_string();
    config.exec_timeout = Duration::from_secs(5);
    config.sub_agent_timeout = Duration::from_secs(1);

    let spec = SubAgentSpec {
        name: "timeout-agent".into(),
        description: String::new(),
        system_prompt: "Run the delegated command.".into(),
        max_turns: 1,
        tools: ToolPermissions {
            allow: vec!["exec".into()],
            deny: vec![],
        },
        mcp_policy: None,
        source: AgentSource::System,
        path: String::new(),
    };

    let (live_tx, _live_rx) = tokio::sync::mpsc::channel(16);
    let called = Arc::new(AtomicBool::new(false));
    let mut hooks = HookRegistry::new();
    hooks.register(Box::new(ObservedTimeoutHook {
        expected_command: command,
        called: called.clone(),
    }));

    let http = reqwest::Client::new();
    let workspace = std::env::temp_dir();
    let outcome = crate::subagents::executor::run_subagent(
        &spec,
        "Run the slow command.",
        &config,
        &http,
        &workspace,
        &live_tx,
        tokio_util::sync::CancellationToken::new(),
        &hooks,
        "test-task-timeout",
    )
    .await;

    handle.join().expect("server thread should join");

    assert!(called.load(Ordering::SeqCst));
    assert!(outcome.aborted);
    assert_eq!(outcome.cycles, 1);
    assert_eq!(outcome.tool_calls, 1);
    assert!(outcome.result.contains("timed out after"));
}

#[tokio::test]
async fn run_subagent_executes_mcp_tool_allowed_by_policy() {
    let workspace = unique_temp_workspace("lingclaw-subagent-mcp-policy");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should exist");
    let log_path = workspace.join("mock.log");

    let mut config = base_config_with_mock_mcp_server("normal", &log_path);
    let reports = crate::tools::mcp::inspect_servers(&config, &workspace).await;
    assert_eq!(reports.len(), 1);
    assert!(reports[0].error.is_none(), "{:?}", reports[0].error);
    let tool_name = reports[0]
        .tool_names
        .first()
        .cloned()
        .expect("mock MCP server should expose a tool");

    let response_body = build_openai_tool_call_stream(
        &tool_name,
        serde_json::json!({
            "value": "left"
        }),
    );
    let (api_base, handle) = spawn_one_shot_http_server("text/event-stream", response_body);
    config.api_base = api_base;
    config.api_key = "test-key".to_string();

    let spec = SubAgentSpec {
        name: "mcp-reader".into(),
        description: String::new(),
        system_prompt: "Use the delegated MCP tool when needed.".into(),
        max_turns: 1,
        tools: ToolPermissions {
            allow: vec!["think".into(), "read_file".into()],
            deny: vec![],
        },
        mcp_policy: Some(McpPolicy::ReadOnly),
        source: AgentSource::System,
        path: String::new(),
    };

    let (live_tx, _live_rx) = tokio::sync::mpsc::channel(16);
    let http = reqwest::Client::new();
    let outcome = crate::subagents::executor::run_subagent(
        &spec,
        "Call the available read-only MCP tool.",
        &config,
        &http,
        &workspace,
        &live_tx,
        tokio_util::sync::CancellationToken::new(),
        &HookRegistry::new(),
        "test-task-mcp",
    )
    .await;

    handle.join().expect("server thread should join");

    let tools_call_count = log_line_count(&log_path, "tools/call");

    let _ = crate::tools::mcp::refresh_servers(&config, &workspace).await;
    let _ = fs::remove_dir_all(&workspace);

    assert_eq!(outcome.cycles, 1);
    assert_eq!(outcome.tool_calls, 1);
    assert!(!outcome.aborted);
    assert_eq!(tools_call_count, 1);
}

// ══════════════════════════════════════════════════════════════════════════════
//  Orchestrator Tests
// ══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_interpolate_results_basic() {
    let mut completed = std::collections::HashMap::new();
    completed.insert("explore".to_string(), "Found 3 files".to_string());
    completed.insert("research".to_string(), "API docs summary".to_string());

    let prompt = "Review the findings: {{results.explore}} and {{results.research}}";
    let result = interpolate_results(prompt, &completed);
    assert_eq!(
        result,
        "Review the findings: Found 3 files and API docs summary"
    );
}

#[test]
fn test_interpolate_results_no_placeholders() {
    let completed = std::collections::HashMap::new();
    let prompt = "Do something without dependencies";
    let result = interpolate_results(prompt, &completed);
    assert_eq!(result, "Do something without dependencies");
}

#[test]
fn test_interpolate_results_missing_reference() {
    let completed = std::collections::HashMap::new();
    let prompt = "Use {{results.missing}} here";
    let result = interpolate_results(prompt, &completed);
    assert_eq!(result, "Use {{results.missing}} here");
}

#[test]
fn test_interpolate_results_does_not_recurse_into_values() {
    let mut completed = std::collections::HashMap::new();
    completed.insert("a".to_string(), "{{results.b}}".to_string());
    completed.insert("b".to_string(), "SHOULD_NOT_APPEAR".to_string());
    let prompt = "See {{results.a}}";
    let result = interpolate_results(prompt, &completed);
    assert_eq!(result, "See {{results.b}}");
    assert!(!result.contains("SHOULD_NOT_APPEAR"));
}

#[test]
fn test_interpolate_results_malformed_placeholder() {
    let mut completed = std::collections::HashMap::new();
    completed.insert("ok".to_string(), "good".to_string());
    let prompt = "unclosed {{results.ok and valid {{results.ok}}";
    let result = interpolate_results(prompt, &completed);
    assert!(result.contains("unclosed {{results.ok and valid {{results.ok}}"));
}

#[test]
fn test_interpolate_results_unicode_in_value() {
    let mut completed = std::collections::HashMap::new();
    completed.insert("greet".to_string(), "你好 🌟".to_string());
    let prompt = "Message: {{results.greet}}!";
    let result = interpolate_results(prompt, &completed);
    assert_eq!(result, "Message: 你好 🌟!");
}

#[test]
fn test_has_cycle_no_cycle() {
    let tasks = vec![
        OrchestrationTask {
            id: "a".into(),
            agent: "coder".into(),
            prompt: "".into(),
            depends_on: vec![],
        },
        OrchestrationTask {
            id: "b".into(),
            agent: "reviewer".into(),
            prompt: "".into(),
            depends_on: vec!["a".into()],
        },
        OrchestrationTask {
            id: "c".into(),
            agent: "coder".into(),
            prompt: "".into(),
            depends_on: vec!["b".into()],
        },
    ];
    assert!(!has_cycle(&tasks));
}

#[test]
fn test_has_cycle_with_cycle() {
    let tasks = vec![
        OrchestrationTask {
            id: "a".into(),
            agent: "coder".into(),
            prompt: "".into(),
            depends_on: vec!["c".into()],
        },
        OrchestrationTask {
            id: "b".into(),
            agent: "reviewer".into(),
            prompt: "".into(),
            depends_on: vec!["a".into()],
        },
        OrchestrationTask {
            id: "c".into(),
            agent: "coder".into(),
            prompt: "".into(),
            depends_on: vec!["b".into()],
        },
    ];
    assert!(has_cycle(&tasks));
}

#[test]
fn test_compute_layers_serial() {
    let plan = OrchestrationPlan {
        tasks: vec![
            OrchestrationTask {
                id: "a".into(),
                agent: "coder".into(),
                prompt: "".into(),
                depends_on: vec![],
            },
            OrchestrationTask {
                id: "b".into(),
                agent: "reviewer".into(),
                prompt: "".into(),
                depends_on: vec!["a".into()],
            },
            OrchestrationTask {
                id: "c".into(),
                agent: "coder".into(),
                prompt: "".into(),
                depends_on: vec!["b".into()],
            },
        ],
    };
    let layers = compute_execution_layers(&plan);
    assert_eq!(layers.len(), 3);
    assert_eq!(layers[0], vec![0]);
    assert_eq!(layers[1], vec![1]);
    assert_eq!(layers[2], vec![2]);
}

#[test]
fn test_compute_layers_parallel() {
    let plan = OrchestrationPlan {
        tasks: vec![
            OrchestrationTask {
                id: "explore".into(),
                agent: "explore".into(),
                prompt: "".into(),
                depends_on: vec![],
            },
            OrchestrationTask {
                id: "research".into(),
                agent: "researcher".into(),
                prompt: "".into(),
                depends_on: vec![],
            },
            OrchestrationTask {
                id: "implement".into(),
                agent: "coder".into(),
                prompt: "".into(),
                depends_on: vec!["explore".into(), "research".into()],
            },
        ],
    };
    let layers = compute_execution_layers(&plan);
    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0].len(), 2);
    assert!(layers[0].contains(&0));
    assert!(layers[0].contains(&1));
    assert_eq!(layers[1], vec![2]);
}

#[test]
fn test_compute_layers_diamond() {
    let plan = OrchestrationPlan {
        tasks: vec![
            OrchestrationTask {
                id: "a".into(),
                agent: "explore".into(),
                prompt: "".into(),
                depends_on: vec![],
            },
            OrchestrationTask {
                id: "b".into(),
                agent: "coder".into(),
                prompt: "".into(),
                depends_on: vec!["a".into()],
            },
            OrchestrationTask {
                id: "c".into(),
                agent: "reviewer".into(),
                prompt: "".into(),
                depends_on: vec!["a".into()],
            },
            OrchestrationTask {
                id: "d".into(),
                agent: "coder".into(),
                prompt: "".into(),
                depends_on: vec!["b".into(), "c".into()],
            },
        ],
    };
    let layers = compute_execution_layers(&plan);
    assert_eq!(layers.len(), 3);
    assert_eq!(layers[0], vec![0]);
    assert_eq!(layers[1].len(), 2);
    assert!(layers[1].contains(&1));
    assert!(layers[1].contains(&2));
    assert_eq!(layers[2], vec![3]);
}

#[test]
fn test_validate_plan_infers_dependencies_from_prompt_placeholders() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    // Use a deterministic built-in agent name rather than relying on
    // discover_all_agents() ordering, which is filesystem-dependent.
    let agent_name = "explore".to_string();
    let tasks = vec![
        OrchestrationTask {
            id: "project_spec".into(),
            agent: agent_name.clone(),
            prompt: "Write the project specification".into(),
            depends_on: vec![],
        },
        OrchestrationTask {
            id: "frontend_dev".into(),
            agent: agent_name.clone(),
            prompt: "Build the frontend from {{results.project_spec}}".into(),
            depends_on: vec![],
        },
        OrchestrationTask {
            id: "backend_dev".into(),
            agent: agent_name.clone(),
            prompt: "Build the backend from {{results.project_spec}}".into(),
            depends_on: vec![],
        },
        OrchestrationTask {
            id: "frontend_review".into(),
            agent: agent_name.clone(),
            prompt: "Review {{results.frontend_dev}}".into(),
            depends_on: vec![],
        },
        OrchestrationTask {
            id: "backend_review".into(),
            agent: agent_name.clone(),
            prompt: "Review {{results.backend_dev}}".into(),
            depends_on: vec![],
        },
        OrchestrationTask {
            id: "integration_review".into(),
            agent: agent_name,
            prompt: "Integrate {{results.frontend_review}} with {{results.backend_review}}".into(),
            depends_on: vec![],
        },
    ];

    let plan =
        validate_plan(tasks, workspace).expect("placeholder references should infer dependencies");
    let layers = compute_execution_layers(&plan);

    assert_eq!(plan.tasks[1].depends_on, vec!["project_spec"]);
    assert_eq!(plan.tasks[2].depends_on, vec!["project_spec"]);
    assert_eq!(plan.tasks[3].depends_on, vec!["frontend_dev"]);
    assert_eq!(plan.tasks[4].depends_on, vec!["backend_dev"]);
    assert_eq!(
        plan.tasks[5].depends_on,
        vec!["frontend_review", "backend_review"]
    );
    assert_eq!(layers.len(), 4);
    assert_eq!(layers[0], vec![0]);
    assert_eq!(layers[1].len(), 2);
    assert!(layers[1].contains(&1));
    assert!(layers[1].contains(&2));
    assert_eq!(layers[2].len(), 2);
    assert!(layers[2].contains(&3));
    assert!(layers[2].contains(&4));
    assert_eq!(layers[3], vec![5]);
}

#[test]
fn test_validate_plan_merges_explicit_and_prompt_dependencies_without_duplicates() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let agent_name = "explore".to_string();
    let tasks = vec![
        OrchestrationTask {
            id: "explore".into(),
            agent: agent_name.clone(),
            prompt: "Explore the codebase".into(),
            depends_on: vec![],
        },
        OrchestrationTask {
            id: "research".into(),
            agent: agent_name.clone(),
            prompt: "Research the API".into(),
            depends_on: vec![],
        },
        OrchestrationTask {
            id: "implement".into(),
            agent: agent_name,
            prompt: "Implement using {{results.explore}} and {{results.research}} and {{results.explore}} again".into(),
            depends_on: vec!["explore".into()],
        },
    ];

    let plan =
        validate_plan(tasks, workspace).expect("explicit and inferred dependencies should merge");

    assert_eq!(plan.tasks[2].depends_on, vec!["explore", "research"]);
}

#[test]
fn test_validate_plan_ignores_unknown_prompt_placeholders() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let agent_name = "explore".to_string();
    let tasks = vec![
        OrchestrationTask {
            id: "spec".into(),
            agent: agent_name.clone(),
            prompt: "Write the specification".into(),
            depends_on: vec![],
        },
        OrchestrationTask {
            id: "implement".into(),
            agent: agent_name,
            prompt: "Use {{results.spec}} and keep {{results.literal_example}} verbatim".into(),
            depends_on: vec![],
        },
    ];

    let plan = validate_plan(tasks, workspace)
        .expect("unknown prompt placeholders should not invalidate the plan");

    assert_eq!(plan.tasks[1].depends_on, vec!["spec"]);
}

#[test]
fn test_validate_plan_rejects_non_placeholder_compatible_task_id() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let tasks = vec![OrchestrationTask {
        id: "bad id".into(),
        agent: "explore".into(),
        prompt: "Do work".into(),
        depends_on: vec![],
    }];

    let err = validate_plan(tasks, workspace).expect_err("invalid task id should be rejected");
    assert!(err.contains("must use only ASCII letters, digits, '_' or '-'"));
}

#[test]
fn test_validate_plan_rejects_cycle_introduced_by_prompt_placeholders() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    // task_a implicitly depends on task_b via placeholder,
    // and task_b implicitly depends on task_a — a cycle.
    let tasks = vec![
        OrchestrationTask {
            id: "task_a".into(),
            agent: "explore".into(),
            prompt: "Do work with {{results.task_b}}".into(),
            depends_on: vec![],
        },
        OrchestrationTask {
            id: "task_b".into(),
            agent: "explore".into(),
            prompt: "Do work with {{results.task_a}}".into(),
            depends_on: vec![],
        },
    ];

    let err = validate_plan(tasks, workspace)
        .expect_err("cycle via inferred prompt placeholders should be rejected");
    assert!(err.contains("cycle"), "error should mention cycle: {err}");
}

#[tokio::test]
async fn test_execute_orchestration_cancelled_emits_skipped_events_for_remaining_tasks() {
    let plan = OrchestrationPlan {
        tasks: vec![
            OrchestrationTask {
                id: "first".into(),
                agent: "coder".into(),
                prompt: "noop".into(),
                depends_on: vec![],
            },
            OrchestrationTask {
                id: "second".into(),
                agent: "reviewer".into(),
                prompt: "noop".into(),
                depends_on: vec!["first".into()],
            },
        ],
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (live_tx, mut live_rx) = tokio::sync::mpsc::channel(32);
    let workspace = std::env::temp_dir();
    let http = reqwest::Client::new();
    let hooks = HookRegistry::new();

    let outcome = execute_orchestration(
        &plan,
        &base_config(),
        &http,
        &workspace,
        &live_tx,
        cancel,
        &hooks,
    )
    .await;

    assert!(outcome.aborted);
    assert_eq!(outcome.task_results.len(), 2);
    assert!(
        outcome
            .task_results
            .iter()
            .all(|result| result.status == TaskStatus::Skipped)
    );

    let mut skipped_ids = Vec::new();
    while let Ok(event) = live_rx.try_recv() {
        if event["type"].as_str() == Some("orchestrate_task_skipped") {
            skipped_ids.push(event["id"].as_str().unwrap_or_default().to_string());
        }
    }
    assert_eq!(skipped_ids, vec!["first".to_string(), "second".to_string()]);
}

#[tokio::test]
async fn test_execute_orchestration_failed_task_event_includes_error_text() {
    let workspace = unique_temp_workspace("lingclaw-orchestrate-failed-event");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(workspace.join("agents/runner")).expect("agent dir should exist");
    fs::write(
        workspace.join("agents/runner/AGENT.md"),
        r#"---
name: runner
description: "Runs commands"
max_turns: 1
tools:
  allow: [exec]
  deny: []
---

Run the requested command.
"#,
    )
    .expect("agent file should be written");

    let command = slow_tool_command();
    let response_body =
        build_openai_tool_call_stream("exec", serde_json::json!({ "command": command }));
    let (api_base, handle) = spawn_one_shot_http_server("text/event-stream", response_body);

    let mut config = base_config();
    config.api_base = api_base;
    config.api_key = "test-key".to_string();
    config.exec_timeout = Duration::from_secs(5);
    config.sub_agent_timeout = Duration::from_secs(1);

    let plan = OrchestrationPlan {
        tasks: vec![OrchestrationTask {
            id: "run".into(),
            agent: "runner".into(),
            prompt: "Run the slow command".into(),
            depends_on: vec![],
        }],
    };

    let (live_tx, mut live_rx) = tokio::sync::mpsc::channel(64);
    let http = reqwest::Client::new();
    let hooks = HookRegistry::new();
    let outcome = execute_orchestration(
        &plan,
        &config,
        &http,
        &workspace,
        &live_tx,
        CancellationToken::new(),
        &hooks,
    )
    .await;

    handle.join().expect("server thread should join");

    assert!(!outcome.aborted);
    assert_eq!(outcome.task_results.len(), 1);
    assert_eq!(outcome.task_results[0].status, TaskStatus::Failed);

    let mut failed_error = None;
    while let Ok(event) = live_rx.try_recv() {
        if event["type"].as_str() == Some("orchestrate_task_failed") {
            failed_error = event["error"].as_str().map(|value| value.to_string());
        }
    }
    let failed_error = failed_error.expect("expected orchestrate_task_failed event");
    assert!(failed_error.contains("timed out after") || failed_error.contains("deadline exceeded"));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn test_format_result_basic() {
    let outcome = OrchestrationOutcome {
        task_results: vec![
            TaskResult {
                id: "explore".into(),
                agent: "explore".into(),
                status: TaskStatus::Completed,
                result: "Found relevant files".into(),
                cycles: 3,
                tool_calls: 5,
                duration_ms: 12000,
                input_tokens: 0,
                output_tokens: 0,
                provider_usage: HashMap::new(),
            },
            TaskResult {
                id: "implement".into(),
                agent: "coder".into(),
                status: TaskStatus::Completed,
                result: "Code written".into(),
                cycles: 8,
                tool_calls: 15,
                duration_ms: 45000,
                input_tokens: 0,
                output_tokens: 0,
                provider_usage: HashMap::new(),
            },
        ],
        aborted: false,
    };
    let report = format_orchestration_result(&outcome);
    assert!(report.contains("Orchestration Complete"));
    assert!(report.contains("2 tasks"));
    assert!(report.contains("2 completed"));
    assert!(report.contains("explore"));
    assert!(report.contains("coder"));
}

#[test]
fn test_format_result_with_failures() {
    let outcome = OrchestrationOutcome {
        task_results: vec![
            TaskResult {
                id: "impl".into(),
                agent: "coder".into(),
                status: TaskStatus::Completed,
                result: "Done".into(),
                cycles: 5,
                tool_calls: 10,
                duration_ms: 30000,
                input_tokens: 0,
                output_tokens: 0,
                provider_usage: HashMap::new(),
            },
            TaskResult {
                id: "review".into(),
                agent: "reviewer".into(),
                status: TaskStatus::Failed,
                result: "LLM error".into(),
                cycles: 1,
                tool_calls: 0,
                duration_ms: 5000,
                input_tokens: 0,
                output_tokens: 0,
                provider_usage: HashMap::new(),
            },
            TaskResult {
                id: "fix".into(),
                agent: "coder".into(),
                status: TaskStatus::Skipped,
                result: "Skipped: dependency 'review' failed".into(),
                cycles: 0,
                tool_calls: 0,
                duration_ms: 0,
                input_tokens: 0,
                output_tokens: 0,
                provider_usage: HashMap::new(),
            },
        ],
        aborted: false,
    };
    let report = format_orchestration_result(&outcome);
    assert!(report.contains("1 completed"));
    assert!(report.contains("1 failed"));
    assert!(report.contains("1 skipped"));
    assert!(report.contains("✅"));
    assert!(report.contains("❌"));
    assert!(report.contains("⏭️"));
}

#[test]
fn test_tool_permissions_blocks_orchestrate() {
    let perms = ToolPermissions {
        allow: vec![],
        deny: vec![],
    };
    assert!(!perms.is_allowed("orchestrate"));
}

#[test]
fn test_tool_permissions_blocks_orchestrate_even_if_explicitly_allowed() {
    let perms = ToolPermissions {
        allow: vec!["orchestrate".into()],
        deny: vec![],
    };
    assert!(!perms.is_allowed("orchestrate"));
}

#[test]
fn test_orchestrate_format_result_all_completed() {
    let outcome = OrchestrationOutcome {
        task_results: vec![
            TaskResult {
                id: "explore".into(),
                agent: "explore".into(),
                status: TaskStatus::Completed,
                result: "Found 5 relevant files".into(),
                cycles: 3,
                tool_calls: 5,
                duration_ms: 12000,
                input_tokens: 0,
                output_tokens: 0,
                provider_usage: HashMap::new(),
            },
            TaskResult {
                id: "implement".into(),
                agent: "coder".into(),
                status: TaskStatus::Completed,
                result: "Feature implemented".into(),
                cycles: 8,
                tool_calls: 15,
                duration_ms: 45000,
                input_tokens: 0,
                output_tokens: 0,
                provider_usage: HashMap::new(),
            },
        ],
        aborted: false,
    };
    let report = format_orchestration_result(&outcome);
    assert!(report.contains("Orchestration Complete"));
    assert!(report.contains("2 tasks"));
    assert!(report.contains("2 completed, 0 failed, 0 skipped"));
    assert!(report.contains("explore"));
    assert!(report.contains("coder"));
    assert!(report.contains("Found 5 relevant files"));
    assert!(report.contains("Feature implemented"));
}

#[test]
fn test_orchestrate_format_result_with_skipped() {
    let outcome = OrchestrationOutcome {
        task_results: vec![
            TaskResult {
                id: "impl".into(),
                agent: "coder".into(),
                status: TaskStatus::Failed,
                result: "LLM error".into(),
                cycles: 1,
                tool_calls: 0,
                duration_ms: 5000,
                input_tokens: 0,
                output_tokens: 0,
                provider_usage: HashMap::new(),
            },
            TaskResult {
                id: "review".into(),
                agent: "reviewer".into(),
                status: TaskStatus::Skipped,
                result: "Skipped: dependency 'impl' failed".into(),
                cycles: 0,
                tool_calls: 0,
                duration_ms: 0,
                input_tokens: 0,
                output_tokens: 0,
                provider_usage: HashMap::new(),
            },
        ],
        aborted: false,
    };
    let report = format_orchestration_result(&outcome);
    assert!(report.contains("0 completed, 1 failed, 1 skipped"));
    assert!(report.contains("❌"));
    assert!(report.contains("⏭️"));
}

#[test]
fn test_orchestrate_format_result_aborted() {
    let outcome = OrchestrationOutcome {
        task_results: vec![TaskResult {
            id: "task1".into(),
            agent: "coder".into(),
            status: TaskStatus::Completed,
            result: "Done".into(),
            cycles: 3,
            tool_calls: 5,
            duration_ms: 10000,
            input_tokens: 0,
            output_tokens: 0,
            provider_usage: HashMap::new(),
        }],
        aborted: true,
    };
    let report = format_orchestration_result(&outcome);
    assert!(report.contains("Orchestration Aborted"));
}

#[test]
fn test_orchestrate_provider_usage_aggregates_tasks() {
    let mut first_usage = HashMap::new();
    first_usage.insert("openai".into(), [120, 30]);
    let mut second_usage = HashMap::new();
    second_usage.insert("openai".into(), [80, 20]);
    second_usage.insert("anthropic".into(), [25, 5]);

    let outcome = OrchestrationOutcome {
        task_results: vec![
            TaskResult {
                id: "task-a".into(),
                agent: "coder".into(),
                status: TaskStatus::Completed,
                result: "Done".into(),
                cycles: 2,
                tool_calls: 1,
                duration_ms: 1000,
                input_tokens: 120,
                output_tokens: 30,
                provider_usage: first_usage,
            },
            TaskResult {
                id: "task-b".into(),
                agent: "reviewer".into(),
                status: TaskStatus::Completed,
                result: "Reviewed".into(),
                cycles: 1,
                tool_calls: 0,
                duration_ms: 800,
                input_tokens: 105,
                output_tokens: 25,
                provider_usage: second_usage,
            },
        ],
        aborted: false,
    };

    let usage = outcome.provider_usage();
    assert_eq!(usage["openai"], [200, 50]);
    assert_eq!(usage["anthropic"], [25, 5]);
}

#[test]
fn test_render_agents_catalog_mentions_orchestrate() {
    let agents = vec![SubAgentSpec {
        name: "coder".into(),
        description: "Code writer".into(),
        system_prompt: String::new(),
        max_turns: 15,
        tools: ToolPermissions::default(),
        mcp_policy: None,
        source: AgentSource::System,
        path: String::new(),
    }];
    let catalog = render_agents_catalog(&agents).unwrap();
    assert!(catalog.contains("orchestrate"));
    assert!(catalog.contains("task"));
}
