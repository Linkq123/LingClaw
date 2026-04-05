use crate::subagents::discovery::discover_all_agents;
use crate::subagents::{AgentSource, SubAgentSpec, ToolPermissions, render_agents_catalog};

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
            source: AgentSource::System,
            path: String::new(),
        },
        SubAgentSpec {
            name: "coder".into(),
            description: String::new(),
            system_prompt: String::new(),
            max_turns: 15,
            tools: ToolPermissions::default(),
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
        source: AgentSource::System,
        path: String::new(),
    };
    let tools = crate::subagents::filter_tools_for_agent(&spec);
    assert!(tools.contains(&"read_file".to_string()));
    assert!(tools.contains(&"list_dir".to_string()));
    assert!(!tools.contains(&"exec".to_string()));
    assert!(!tools.contains(&"task".to_string()));
}

// --- Sub-agent model resolution tests ---
// These tests call the production `resolve_subagent_model()` function.

use crate::Config;
use crate::config::Provider;
use crate::subagents::executor::resolve_subagent_model;
use std::collections::HashMap;
use std::time::Duration;

fn base_config() -> Config {
    Config {
        api_key: String::new(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "openai/gpt-4o".to_string(),
        fast_model: None,
        sub_agent_model: None,
        memory_model: None,
        provider: Provider::OpenAI,
        anthropic_prompt_caching: false,
        openai_stream_include_usage: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: 18989,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        tool_timeout: Duration::from_secs(30),
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
        structured_memory: false,
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
