use crate::subagents::discovery::discover_all_agents;
use crate::subagents::{AgentSource, SubAgentSpec, ToolPermissions, render_agents_catalog};
use crate::{ChatMessage, agent};

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
use crate::config::{JsonMcpServerConfig, Provider};
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

fn base_config() -> Config {
    Config {
        api_key: String::new(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "openai/gpt-4o".to_string(),
        fast_model: None,
        sub_agent_model: None,
        memory_model: None,

        reflection_model: None,
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

fn read_http_request(stream: &mut TcpStream) {
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
}

fn spawn_one_shot_http_server(
    response_content_type: &'static str,
    response_body: String,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should expose address");

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("request should connect");
        read_http_request(&mut stream);
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

    (format!("http://{}", address), handle)
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

fn slow_tool_command() -> String {
    if cfg!(windows) {
        "timeout /T 2 /NOBREAK > NUL".to_string()
    } else {
        "while :; do :; done".to_string()
    }
}

/// Sub-agents use the configured delegated model when set.
#[test]
fn test_model_resolution_prefers_sub_agent_config() {
    let config = Config {
        sub_agent_model: Some("openai/gpt-4o-mini".to_string()),
        ..base_config()
    };
    assert_eq!(resolve_subagent_model(&config), "openai/gpt-4o-mini");
}

/// Falls back to config.model when no dedicated sub-agent model is configured.
#[test]
fn test_model_resolution_falls_back_to_primary() {
    let config = base_config(); // sub_agent_model = None
    assert_eq!(resolve_subagent_model(&config), "openai/gpt-4o");
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
