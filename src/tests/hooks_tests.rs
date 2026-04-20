use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use super::*;
use crate::DEFAULT_PORT;
use crate::config;

// ── Test helpers ─────────────────────────────────────────────────────────────

fn test_config() -> Config {
    Config {
        api_key: "test-key".to_string(),
        api_base: "https://test.example/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        fast_model: None,
        sub_agent_model: None,
        memory_model: None,

        reflection_model: None,
        context_model: None,
        provider: crate::config::Provider::OpenAI,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: DEFAULT_PORT,
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

fn test_tool_input(name: &str, args: serde_json::Value) -> ToolHookInput {
    ToolHookInput {
        tool_name: name.to_string(),
        tool_args: args,
        tool_id: "tc-001".to_string(),
        cycle: 0,
        workspace: PathBuf::from("/tmp/test"),
        outcome_output: None,
        outcome_is_error: None,
        outcome_duration_ms: None,
    }
}

fn test_tool_input_after(name: &str, output: &str, is_error: bool) -> ToolHookInput {
    ToolHookInput {
        tool_name: name.to_string(),
        tool_args: serde_json::json!({}),
        tool_id: "tc-001".to_string(),
        cycle: 0,
        workspace: PathBuf::from("/tmp/test"),
        outcome_output: Some(output.to_string()),
        outcome_is_error: Some(is_error),
        outcome_duration_ms: Some(42),
    }
}

// ── Stub hooks ───────────────────────────────────────────────────────────────

/// A tool hook that rejects any tool named "dangerous_tool".
struct RejectDangerousHook;

impl AgentHook for RejectDangerousHook {
    fn name(&self) -> &'static str {
        "reject_dangerous"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeToolExec
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
        point == agent::HookPoint::BeforeToolExec && tool_name == "dangerous_tool"
    }
    fn run_tool<'a>(
        &'a self,
        _input: ToolHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async {
            HookOutput::Reject {
                reason: "dangerous tool blocked".to_string(),
                events: vec![],
            }
        })
    }
}

/// A tool hook that uppercases the tool result.
struct UppercaseResultHook;

impl AgentHook for UppercaseResultHook {
    fn name(&self) -> &'static str {
        "uppercase_result"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::AfterToolExec
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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

    fn should_run_tool(&self, _tool_name: &str, point: agent::HookPoint) -> bool {
        point == agent::HookPoint::AfterToolExec
    }
    fn run_tool<'a>(
        &'a self,
        input: ToolHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async move {
            if let Some(output) = input.outcome_output {
                HookOutput::ModifyToolResult {
                    result: output.to_uppercase(),
                }
            } else {
                HookOutput::NoOp
            }
        })
    }
}

/// A tool hook that injects extra args.
struct InjectArgsHook;

impl AgentHook for InjectArgsHook {
    fn name(&self) -> &'static str {
        "inject_args"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeToolExec
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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

    fn should_run_tool(&self, _tool_name: &str, point: agent::HookPoint) -> bool {
        point == agent::HookPoint::BeforeToolExec
    }
    fn run_tool<'a>(
        &'a self,
        input: ToolHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async move {
            let mut args = input.tool_args;
            if let Some(obj) = args.as_object_mut() {
                obj.insert("injected".to_string(), serde_json::json!(true));
            }
            HookOutput::ModifyToolArgs { args }
        })
    }
}

/// An LLM hook that appends extra system text.
struct ExtraSystemHook {
    extra: String,
}

impl AgentHook for ExtraSystemHook {
    fn name(&self) -> &'static str {
        "extra_system"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeLlmCall
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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

    fn should_run_llm(&self, _cycle: usize) -> bool {
        true
    }
    fn run_llm<'a>(
        &'a self,
        _input: LlmHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        let extra = self.extra.clone();
        Box::pin(async move {
            HookOutput::ModifyLlmParams {
                extra_system: Some(extra),
                think_override: None,
            }
        })
    }
}

/// A command hook that emits an event for /help.
struct HelpAuditHook;

impl AgentHook for HelpAuditHook {
    fn name(&self) -> &'static str {
        "help_audit"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::OnCommand
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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

    fn should_run_command(&self, command: &str) -> bool {
        command == "/help"
    }
    fn run_command<'a>(
        &'a self,
        input: CommandHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>> {
        Box::pin(async move {
            vec![serde_json::json!({
                "type": "hook_audit",
                "command": input.command,
            })]
        })
    }
}

/// A hook that declares point() = OnFinish but overrides should_run_tool to return true.
/// Used to test point() enforcement in the dispatch functions.
struct WrongPointToolHook;

impl AgentHook for WrongPointToolHook {
    fn name(&self) -> &'static str {
        "wrong_point_tool"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::OnFinish // ← wrong category for tool hooks
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
    fn should_run_tool(&self, _tool_name: &str, _point: agent::HookPoint) -> bool {
        true // would fire if point() not checked
    }
    fn run_tool<'a>(
        &'a self,
        _input: ToolHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async {
            HookOutput::Reject {
                reason: "should never fire".to_string(),
                events: vec![],
            }
        })
    }
}

/// A hook that declares point() = OnFinish but overrides should_run_llm to return true.
struct WrongPointLlmHook;

impl AgentHook for WrongPointLlmHook {
    fn name(&self) -> &'static str {
        "wrong_point_llm"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::OnFinish
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
    fn should_run_llm(&self, _cycle: usize) -> bool {
        true
    }
    fn run_llm<'a>(
        &'a self,
        _input: LlmHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async {
            HookOutput::ModifyLlmParams {
                extra_system: Some("should never fire".to_string()),
                think_override: None,
            }
        })
    }
}

/// A hook that declares point() = BeforeAnalyze but overrides should_run_command to return true.
struct WrongPointCommandHook;

impl AgentHook for WrongPointCommandHook {
    fn name(&self) -> &'static str {
        "wrong_point_command"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeAnalyze
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
    fn should_run_command(&self, _command: &str) -> bool {
        true
    }
    fn run_command<'a>(
        &'a self,
        _input: CommandHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>> {
        Box::pin(async { vec![serde_json::json!({"type": "should_never_fire"})] })
    }
}

/// An AfterToolExec hook that captures the tool_args it receives (for asserting
/// effective_args propagation).
struct CaptureArgsAfterHook {
    captured_args: std::sync::Mutex<Option<serde_json::Value>>,
}

impl CaptureArgsAfterHook {
    fn new() -> Self {
        Self {
            captured_args: std::sync::Mutex::new(None),
        }
    }
    fn captured(&self) -> Option<serde_json::Value> {
        self.captured_args.lock().unwrap().clone()
    }
}

impl AgentHook for CaptureArgsAfterHook {
    fn name(&self) -> &'static str {
        "capture_args_after"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::AfterToolExec
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
    fn should_run_tool(&self, _tool_name: &str, point: agent::HookPoint) -> bool {
        point == agent::HookPoint::AfterToolExec
    }
    fn run_tool<'a>(
        &'a self,
        input: ToolHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async move {
            *self.captured_args.lock().unwrap() = Some(input.tool_args.clone());
            HookOutput::NoOp
        })
    }
}

// ── Tests: run_tool_hooks ────────────────────────────────────────────────────

#[tokio::test]
async fn run_tool_hooks_noop_when_no_hooks_registered() {
    let registry = HookRegistry::new();
    let config = test_config();
    let input = test_tool_input("read_file", serde_json::json!({"path": "/a.txt"}));
    let output = run_tool_hooks(&registry, agent::HookPoint::BeforeToolExec, input, &config).await;
    assert!(matches!(output, HookOutput::NoOp));
}

#[tokio::test]
async fn run_tool_hooks_reject_short_circuits() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(RejectDangerousHook));
    let config = test_config();
    let input = test_tool_input("dangerous_tool", serde_json::json!({}));
    let output = run_tool_hooks(&registry, agent::HookPoint::BeforeToolExec, input, &config).await;
    match output {
        HookOutput::Reject { reason, .. } => {
            assert!(reason.contains("dangerous tool blocked"));
        }
        _ => panic!("expected Reject"),
    }
}

#[tokio::test]
async fn run_tool_hooks_reject_skips_non_matching_tool() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(RejectDangerousHook));
    let config = test_config();
    let input = test_tool_input("read_file", serde_json::json!({}));
    let output = run_tool_hooks(&registry, agent::HookPoint::BeforeToolExec, input, &config).await;
    assert!(matches!(output, HookOutput::NoOp));
}

#[tokio::test]
async fn run_tool_hooks_modify_args() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(InjectArgsHook));
    let config = test_config();
    let input = test_tool_input("some_tool", serde_json::json!({"key": "val"}));
    let output = run_tool_hooks(&registry, agent::HookPoint::BeforeToolExec, input, &config).await;
    match output {
        HookOutput::ModifyToolArgs { args } => {
            assert_eq!(args["key"], "val");
            assert_eq!(args["injected"], true);
        }
        _ => panic!("expected ModifyToolArgs"),
    }
}

#[tokio::test]
async fn run_tool_hooks_modify_result() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(UppercaseResultHook));
    let config = test_config();
    let input = test_tool_input_after("read_file", "hello world", false);
    let output = run_tool_hooks(&registry, agent::HookPoint::AfterToolExec, input, &config).await;
    match output {
        HookOutput::ModifyToolResult { result } => {
            assert_eq!(result, "HELLO WORLD");
        }
        _ => panic!("expected ModifyToolResult"),
    }
}

#[tokio::test]
async fn run_tool_hooks_skips_wrong_point_hook() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(WrongPointToolHook));
    let config = test_config();
    let input = test_tool_input("anything", serde_json::json!({}));
    let output = run_tool_hooks(&registry, agent::HookPoint::BeforeToolExec, input, &config).await;
    // WrongPointToolHook declares OnFinish, so it must be skipped despite should_run_tool=true
    assert!(matches!(output, HookOutput::NoOp));
}

#[tokio::test]
async fn run_tool_hooks_after_sees_effective_args() {
    // Simulate the runtime pattern: BeforeToolExec modifies args, then AfterToolExec
    // should see the modified args (not the originals).
    let mut registry = HookRegistry::new();
    let capture = std::sync::Arc::new(CaptureArgsAfterHook::new());
    registry.register(Box::new(InjectArgsHook)); // not used for AfterToolExec, just in registry
    // We can't chain hooks across points in dispatch, so directly verify AfterToolExec
    // receives whatever tool_args are given.
    let after_registry = {
        let mut r = HookRegistry::new();
        // Use a wrapper to share Arc
        struct ArcCaptureHook(std::sync::Arc<CaptureArgsAfterHook>);
        impl AgentHook for ArcCaptureHook {
            fn name(&self) -> &'static str {
                self.0.name()
            }
            fn point(&self) -> agent::HookPoint {
                self.0.point()
            }
            fn should_run(
                &self,
                a: &[ChatMessage],
                b: config::Provider,
                c: usize,
                d: usize,
            ) -> bool {
                self.0.should_run(a, b, c, d)
            }
            fn run<'a>(
                &'a self,
                i: HookInput,
                c: &'a Config,
                h: &'a reqwest::Client,
            ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
                self.0.run(i, c, h)
            }
            fn should_run_tool(&self, n: &str, p: agent::HookPoint) -> bool {
                self.0.should_run_tool(n, p)
            }
            fn run_tool<'a>(
                &'a self,
                i: ToolHookInput,
                c: &'a Config,
            ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
                self.0.run_tool(i, c)
            }
        }
        r.register(Box::new(ArcCaptureHook(capture.clone())));
        r
    };
    let config = test_config();

    // The effective args after BeforeToolExec would have {"key":"val","injected":true}.
    // We pass those as the input to AfterToolExec, simulating the runtime propagation.
    let after_input = ToolHookInput {
        tool_name: "some_tool".to_string(),
        tool_args: serde_json::json!({"key": "val", "injected": true}),
        tool_id: "tc-001".to_string(),
        cycle: 0,
        workspace: PathBuf::from("/tmp/test"),
        outcome_output: Some("done".to_string()),
        outcome_is_error: Some(false),
        outcome_duration_ms: Some(10),
    };
    let _ = run_tool_hooks(
        &after_registry,
        agent::HookPoint::AfterToolExec,
        after_input,
        &config,
    )
    .await;
    let captured = capture
        .captured()
        .expect("AfterToolExec hook should have fired");
    assert_eq!(
        captured["injected"], true,
        "AfterToolExec should see effective args"
    );
    assert_eq!(captured["key"], "val");
}

#[tokio::test]
async fn run_tool_hooks_after_skipped_for_rejected_tool() {
    // When a tool is rejected, record_tool_result passes effective_args=None,
    // so AfterToolExec hooks should never fire.  At the dispatch level, this
    // means we simply don't call run_tool_hooks at all for the rejected case.
    // We verify: if we *do* call run_tool_hooks with a BeforeToolExec-only
    // registry at AfterToolExec, it returns NoOp (the hook's point doesn't match).
    let mut registry = HookRegistry::new();
    registry.register(Box::new(RejectDangerousHook));
    let config = test_config();
    let input = test_tool_input_after("dangerous_tool", "[rejected by hook] blocked", true);
    let output = run_tool_hooks(&registry, agent::HookPoint::AfterToolExec, input, &config).await;
    // RejectDangerousHook declares BeforeToolExec, so it is skipped at AfterToolExec
    assert!(matches!(output, HookOutput::NoOp));
}

// ── Tests: run_llm_hooks ─────────────────────────────────────────────────────

#[tokio::test]
async fn run_llm_hooks_noop_when_empty() {
    let registry = HookRegistry::new();
    let config = test_config();
    let input = LlmHookInput {
        messages: vec![],
        model: "gpt-4o".to_string(),
        think_level: "none".to_string(),
        cycle: 0,
        tool_count: 0,
    };
    let output = run_llm_hooks(&registry, &input, &config).await;
    assert!(matches!(output, HookOutput::NoOp));
}

#[tokio::test]
async fn run_llm_hooks_extra_system_appended() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(ExtraSystemHook {
        extra: "Be concise.".to_string(),
    }));
    let config = test_config();
    let input = LlmHookInput {
        messages: vec![],
        model: "gpt-4o".to_string(),
        think_level: "none".to_string(),
        cycle: 0,
        tool_count: 0,
    };
    let output = run_llm_hooks(&registry, &input, &config).await;
    match output {
        HookOutput::ModifyLlmParams {
            extra_system,
            think_override,
        } => {
            assert_eq!(extra_system.unwrap(), "Be concise.");
            assert!(think_override.is_none());
        }
        _ => panic!("expected ModifyLlmParams"),
    }
}

#[tokio::test]
async fn run_llm_hooks_multiple_extra_system_concatenated() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(ExtraSystemHook {
        extra: "Rule 1.".to_string(),
    }));
    registry.register(Box::new(ExtraSystemHook {
        extra: "Rule 2.".to_string(),
    }));
    let config = test_config();
    let input = LlmHookInput {
        messages: vec![],
        model: "gpt-4o".to_string(),
        think_level: "none".to_string(),
        cycle: 0,
        tool_count: 0,
    };
    let output = run_llm_hooks(&registry, &input, &config).await;
    match output {
        HookOutput::ModifyLlmParams { extra_system, .. } => {
            let text = extra_system.unwrap();
            assert!(text.contains("Rule 1."));
            assert!(text.contains("Rule 2."));
        }
        _ => panic!("expected ModifyLlmParams"),
    }
}

#[tokio::test]
async fn run_llm_hooks_skips_wrong_point_hook() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(WrongPointLlmHook));
    let config = test_config();
    let input = LlmHookInput {
        messages: vec![],
        model: "gpt-4o".to_string(),
        think_level: "none".to_string(),
        cycle: 0,
        tool_count: 0,
    };
    let output = run_llm_hooks(&registry, &input, &config).await;
    // WrongPointLlmHook declares OnFinish, must be skipped
    assert!(matches!(output, HookOutput::NoOp));
}

// ── Tests: run_llm_hooks (chaining) ──────────────────────────────────────────

/// An LLM hook that overrides think_level.
struct ThinkOverrideHook {
    new_level: String,
}

impl AgentHook for ThinkOverrideHook {
    fn name(&self) -> &'static str {
        "think_override"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeLlmCall
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
    fn should_run_llm(&self, _cycle: usize) -> bool {
        true
    }
    fn run_llm<'a>(
        &'a self,
        _input: LlmHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        let level = self.new_level.clone();
        Box::pin(async move {
            HookOutput::ModifyLlmParams {
                extra_system: None,
                think_override: Some(level),
            }
        })
    }
}

/// An LLM hook that captures the think_level it receives in its input.
struct CaptureThinkLevelHook {
    captured: std::sync::Mutex<Option<String>>,
}

impl CaptureThinkLevelHook {
    fn new() -> Self {
        Self {
            captured: std::sync::Mutex::new(None),
        }
    }
    fn captured(&self) -> Option<String> {
        self.captured.lock().unwrap().clone()
    }
}

impl AgentHook for CaptureThinkLevelHook {
    fn name(&self) -> &'static str {
        "capture_think_level"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeLlmCall
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
    fn should_run_llm(&self, _cycle: usize) -> bool {
        true
    }
    fn run_llm<'a>(
        &'a self,
        input: LlmHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async move {
            *self.captured.lock().unwrap() = Some(input.think_level.clone());
            HookOutput::NoOp
        })
    }
}

#[tokio::test]
async fn run_llm_hooks_chained_think_override_visible_to_next_hook() {
    // Hook A overrides think_level to "high".
    // Hook B (registered second) should see think_level="high" in its input.
    let mut registry = HookRegistry::new();
    registry.register(Box::new(ThinkOverrideHook {
        new_level: "high".to_string(),
    }));

    // Use Arc wrapper so we can inspect after dispatch.
    let capture = std::sync::Arc::new(CaptureThinkLevelHook::new());
    struct ArcCaptureThinkHook(std::sync::Arc<CaptureThinkLevelHook>);
    impl AgentHook for ArcCaptureThinkHook {
        fn name(&self) -> &'static str {
            self.0.name()
        }
        fn point(&self) -> agent::HookPoint {
            self.0.point()
        }
        fn should_run(&self, a: &[ChatMessage], b: config::Provider, c: usize, d: usize) -> bool {
            self.0.should_run(a, b, c, d)
        }
        fn run<'a>(
            &'a self,
            i: HookInput,
            c: &'a Config,
            h: &'a reqwest::Client,
        ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
            self.0.run(i, c, h)
        }
        fn should_run_llm(&self, cycle: usize) -> bool {
            self.0.should_run_llm(cycle)
        }
        fn run_llm<'a>(
            &'a self,
            i: LlmHookInput,
            c: &'a Config,
        ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
            self.0.run_llm(i, c)
        }
    }
    registry.register(Box::new(ArcCaptureThinkHook(capture.clone())));

    let config = test_config();
    let input = LlmHookInput {
        messages: vec![],
        model: "gpt-4o".to_string(),
        think_level: "off".to_string(), // original level
        cycle: 0,
        tool_count: 0,
    };
    let output = run_llm_hooks(&registry, &input, &config).await;

    // Hook A's override should propagate to the final result.
    match &output {
        HookOutput::ModifyLlmParams { think_override, .. } => {
            assert_eq!(think_override.as_deref(), Some("high"));
        }
        _ => panic!("expected ModifyLlmParams"),
    }

    // Hook B should have seen "high" (from hook A), not "off" (original).
    let seen = capture.captured().expect("hook B should have fired");
    assert_eq!(
        seen, "high",
        "subsequent hook must see accumulated think_override"
    );
}

/// An LLM hook that captures the messages it receives (for asserting
/// extra_system propagation to subsequent hooks).
struct CaptureMessagesLlmHook {
    captured_messages: std::sync::Mutex<Option<Vec<ChatMessage>>>,
}

impl CaptureMessagesLlmHook {
    fn new() -> Self {
        Self {
            captured_messages: std::sync::Mutex::new(None),
        }
    }
    fn captured(&self) -> Option<Vec<ChatMessage>> {
        self.captured_messages.lock().unwrap().clone()
    }
}

impl AgentHook for CaptureMessagesLlmHook {
    fn name(&self) -> &'static str {
        "capture_messages_llm"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeLlmCall
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
    fn should_run_llm(&self, _cycle: usize) -> bool {
        true
    }
    fn run_llm<'a>(
        &'a self,
        input: LlmHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async move {
            *self.captured_messages.lock().unwrap() = Some(input.messages);
            HookOutput::NoOp
        })
    }
}

#[tokio::test]
async fn run_llm_hooks_chained_extra_system_visible_to_next_hook() {
    // Hook A injects extra_system "Rule 1.".
    // Hook B (registered second) should see "Rule 1." in its system message.
    let mut registry = HookRegistry::new();
    registry.register(Box::new(ExtraSystemHook {
        extra: "Rule 1.".to_string(),
    }));

    let capture = std::sync::Arc::new(CaptureMessagesLlmHook::new());
    struct ArcCaptureMsgsHook(std::sync::Arc<CaptureMessagesLlmHook>);
    impl AgentHook for ArcCaptureMsgsHook {
        fn name(&self) -> &'static str {
            self.0.name()
        }
        fn point(&self) -> agent::HookPoint {
            self.0.point()
        }
        fn should_run(&self, a: &[ChatMessage], b: config::Provider, c: usize, d: usize) -> bool {
            self.0.should_run(a, b, c, d)
        }
        fn run<'a>(
            &'a self,
            i: HookInput,
            c: &'a Config,
            h: &'a reqwest::Client,
        ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
            self.0.run(i, c, h)
        }
        fn should_run_llm(&self, cycle: usize) -> bool {
            self.0.should_run_llm(cycle)
        }
        fn run_llm<'a>(
            &'a self,
            i: LlmHookInput,
            c: &'a Config,
        ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
            self.0.run_llm(i, c)
        }
    }
    registry.register(Box::new(ArcCaptureMsgsHook(capture.clone())));

    let config = test_config();
    let input = LlmHookInput {
        messages: vec![ChatMessage {
            role: "system".to_string(),
            content: Some("Base system prompt.".to_string()),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        }],
        model: "gpt-4o".to_string(),
        think_level: "none".to_string(),
        cycle: 0,
        tool_count: 0,
    };
    let output = run_llm_hooks(&registry, &input, &config).await;

    // Final extra_system should contain "Rule 1.".
    match &output {
        HookOutput::ModifyLlmParams { extra_system, .. } => {
            assert_eq!(extra_system.as_deref(), Some("Rule 1."));
        }
        _ => panic!("expected ModifyLlmParams"),
    }

    // Hook B should have seen the system message with "Rule 1." already appended.
    let seen = capture.captured().expect("hook B should have fired");
    assert_eq!(seen[0].role, "system");
    let sys_content = seen[0].content.as_deref().unwrap();
    assert!(
        sys_content.contains("Rule 1."),
        "subsequent hook must see prior hook's extra_system in messages, got: {sys_content}"
    );
    assert!(sys_content.contains("Base system prompt."));
}

// ── Tests: re-prune after hook modification ──────────────────────────────────

#[tokio::test]
async fn prune_messages_trims_local_snapshot_to_fit_budget() {
    // Simulate the runtime re-prune path: after a hook injects extra_system,
    // the estimate may exceed the budget, so we re-prune a local Vec.
    use crate::config::Provider;
    use crate::context::{estimate_tokens_for_provider, prune_messages_for_provider};

    let mut messages = vec![
        ChatMessage {
            role: "system".to_string(),
            content: Some("System prompt.".to_string()),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some("First question with some context padding text.".to_string()),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".to_string(),
            content: Some("First answer with extra padding to push tokens.".to_string()),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".to_string(),
            content: Some("Second question.".to_string()),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".to_string(),
            content: Some("Second answer.".to_string()),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let provider = Provider::OpenAI;
    let original_estimate = estimate_tokens_for_provider(provider, &messages);
    assert!(original_estimate > 0);

    // Use a tight budget that should force at least one turn to be pruned.
    let tight_budget = original_estimate / 2;
    prune_messages_for_provider(&mut messages, provider, tight_budget);

    let after_estimate = estimate_tokens_for_provider(provider, &messages);
    assert!(
        after_estimate <= tight_budget,
        "after re-prune, estimate ({after_estimate}) must fit budget ({tight_budget})"
    );
    // System message must always survive.
    assert_eq!(messages[0].role, "system");
    assert!(
        messages.len() >= 2,
        "at least system + one message must survive"
    );
}

// ── Tests: run_command_hooks ─────────────────────────────────────────────────

#[tokio::test]
async fn run_command_hooks_noop_when_empty() {
    let registry = HookRegistry::new();
    let config = test_config();
    let input = CommandHookInput {
        command: "/help".to_string(),
        args: String::new(),
        result_type: "system".to_string(),
        session_id: "main".to_string(),
    };
    let events = run_command_hooks(&registry, &input, &config).await;
    assert!(events.is_empty());
}

#[tokio::test]
async fn run_command_hooks_fires_for_matching_command() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(HelpAuditHook));
    let config = test_config();
    let input = CommandHookInput {
        command: "/help".to_string(),
        args: String::new(),
        result_type: "system".to_string(),
        session_id: "main".to_string(),
    };
    let events = run_command_hooks(&registry, &input, &config).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "hook_audit");
    assert_eq!(events[0]["command"], "/help");
}

#[tokio::test]
async fn run_command_hooks_skips_non_matching_command() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(HelpAuditHook));
    let config = test_config();
    let input = CommandHookInput {
        command: "/new".to_string(),
        args: String::new(),
        result_type: "system".to_string(),
        session_id: "main".to_string(),
    };
    let events = run_command_hooks(&registry, &input, &config).await;
    assert!(events.is_empty());
}

#[tokio::test]
async fn run_command_hooks_skips_wrong_point_hook() {
    let mut registry = HookRegistry::new();
    registry.register(Box::new(WrongPointCommandHook));
    let config = test_config();
    let input = CommandHookInput {
        command: "/help".to_string(),
        args: String::new(),
        result_type: "system".to_string(),
        session_id: "main".to_string(),
    };
    let events = run_command_hooks(&registry, &input, &config).await;
    // WrongPointCommandHook declares BeforeAnalyze, must be skipped
    assert!(events.is_empty());
}

// ── Tests: output-type validation in run_tool_hooks ──────────────────────────

/// A BeforeToolExec hook that incorrectly returns ModifyToolResult.
struct InvalidOutputBeforeHook;

impl AgentHook for InvalidOutputBeforeHook {
    fn name(&self) -> &'static str {
        "invalid_output_before"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeToolExec
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
    fn should_run_tool(&self, _tool_name: &str, point: agent::HookPoint) -> bool {
        point == agent::HookPoint::BeforeToolExec
    }
    fn run_tool<'a>(
        &'a self,
        _input: ToolHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        // Wrong: returns ModifyToolResult during BeforeToolExec
        Box::pin(async {
            HookOutput::ModifyToolResult {
                result: "bad".to_string(),
            }
        })
    }
}

/// An AfterToolExec hook that incorrectly returns Reject.
struct InvalidRejectAfterHook;

impl AgentHook for InvalidRejectAfterHook {
    fn name(&self) -> &'static str {
        "invalid_reject_after"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::AfterToolExec
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
    fn should_run_tool(&self, _tool_name: &str, point: agent::HookPoint) -> bool {
        point == agent::HookPoint::AfterToolExec
    }
    fn run_tool<'a>(
        &'a self,
        _input: ToolHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        // Wrong: returns Reject during AfterToolExec
        Box::pin(async {
            HookOutput::Reject {
                reason: "too late".to_string(),
                events: vec![],
            }
        })
    }
}

/// An AfterToolExec hook that incorrectly returns ModifyToolArgs.
struct InvalidModifyArgsAfterHook;

impl AgentHook for InvalidModifyArgsAfterHook {
    fn name(&self) -> &'static str {
        "invalid_modify_args_after"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::AfterToolExec
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
    fn should_run_tool(&self, _tool_name: &str, point: agent::HookPoint) -> bool {
        point == agent::HookPoint::AfterToolExec
    }
    fn run_tool<'a>(
        &'a self,
        _input: ToolHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        // Wrong: returns ModifyToolArgs during AfterToolExec
        Box::pin(async {
            HookOutput::ModifyToolArgs {
                args: serde_json::json!({"bad": true}),
            }
        })
    }
}

#[tokio::test]
async fn run_tool_hooks_invalid_modify_result_during_before_treated_as_noop() {
    // A BeforeToolExec hook returning ModifyToolResult should be silently
    // ignored (treated as NoOp) in release mode.
    let mut registry = HookRegistry::new();
    registry.register(Box::new(InvalidOutputBeforeHook));
    let config = test_config();
    let input = test_tool_input("some_tool", serde_json::json!({"key": "val"}));
    let output = run_tool_hooks(&registry, agent::HookPoint::BeforeToolExec, input, &config).await;
    // Invalid output must not modify anything — result should be NoOp
    assert!(matches!(output, HookOutput::NoOp));
}

#[tokio::test]
async fn run_tool_hooks_invalid_reject_during_after_treated_as_noop() {
    // An AfterToolExec hook returning Reject should be silently ignored.
    let mut registry = HookRegistry::new();
    registry.register(Box::new(InvalidRejectAfterHook));
    let config = test_config();
    let input = test_tool_input_after("some_tool", "result text", false);
    let output = run_tool_hooks(&registry, agent::HookPoint::AfterToolExec, input, &config).await;
    // Reject must not short-circuit during AfterToolExec
    assert!(matches!(output, HookOutput::NoOp));
}

#[tokio::test]
async fn run_tool_hooks_invalid_modify_args_during_after_treated_as_noop() {
    // An AfterToolExec hook returning ModifyToolArgs should be silently ignored.
    let mut registry = HookRegistry::new();
    registry.register(Box::new(InvalidModifyArgsAfterHook));
    let config = test_config();
    let input = test_tool_input_after("some_tool", "result text", false);
    let output = run_tool_hooks(&registry, agent::HookPoint::AfterToolExec, input, &config).await;
    assert!(matches!(output, HookOutput::NoOp));
}

// ── Tests: unknown_command result_type in command hooks ───────────────────────

#[tokio::test]
async fn run_command_hooks_fires_for_unknown_command_result_type() {
    // Simulates the runtime path where an unknown command fires OnCommand
    // hooks with result_type="unknown_command".
    struct AllCommandAuditHook;
    impl AgentHook for AllCommandAuditHook {
        fn name(&self) -> &'static str {
            "all_command_audit"
        }
        fn point(&self) -> agent::HookPoint {
            agent::HookPoint::OnCommand
        }
        fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
        fn should_run_command(&self, _command: &str) -> bool {
            true
        }
        fn run_command<'a>(
            &'a self,
            input: CommandHookInput,
            _config: &'a Config,
        ) -> Pin<Box<dyn Future<Output = Vec<serde_json::Value>> + Send + 'a>> {
            Box::pin(async move {
                vec![serde_json::json!({
                    "type": "audit",
                    "command": input.command,
                    "result_type": input.result_type,
                })]
            })
        }
    }

    let mut registry = HookRegistry::new();
    registry.register(Box::new(AllCommandAuditHook));
    let config = test_config();
    let input = CommandHookInput {
        command: "/nonexistent".to_string(),
        args: String::new(),
        result_type: "unknown_command".to_string(),
        session_id: "main".to_string(),
    };
    let events = run_command_hooks(&registry, &input, &config).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["command"], "/nonexistent");
    assert_eq!(events[0]["result_type"], "unknown_command");
}

// ── Tests: BeforeToolExec chaining ───────────────────────────────────────────

/// A second BeforeToolExec hook that captures the args it receives (to verify
/// it sees the modifications from a prior hook).
struct CaptureArgsBeforeHook {
    captured: std::sync::Mutex<Option<serde_json::Value>>,
}

impl CaptureArgsBeforeHook {
    fn new() -> Self {
        Self {
            captured: std::sync::Mutex::new(None),
        }
    }
    fn captured(&self) -> Option<serde_json::Value> {
        self.captured.lock().unwrap().clone()
    }
}

impl AgentHook for CaptureArgsBeforeHook {
    fn name(&self) -> &'static str {
        "capture_args_before"
    }
    fn point(&self) -> agent::HookPoint {
        agent::HookPoint::BeforeToolExec
    }
    fn should_run(&self, _: &[ChatMessage], _: config::Provider, _: usize, _: usize) -> bool {
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
    fn should_run_tool(&self, _tool_name: &str, point: agent::HookPoint) -> bool {
        point == agent::HookPoint::BeforeToolExec
    }
    fn run_tool<'a>(
        &'a self,
        input: ToolHookInput,
        _config: &'a Config,
    ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
        Box::pin(async move {
            *self.captured.lock().unwrap() = Some(input.tool_args.clone());
            // Also add its own field to prove chaining works end-to-end
            let mut args = input.tool_args;
            if let Some(obj) = args.as_object_mut() {
                obj.insert("captured_phase".to_string(), serde_json::json!(true));
            }
            HookOutput::ModifyToolArgs { args }
        })
    }
}

#[tokio::test]
async fn run_tool_hooks_before_chaining_second_hook_sees_modified_args() {
    // Hook A (InjectArgsHook) adds {"injected": true}.
    // Hook B (CaptureArgsBeforeHook) should see {"key": "val", "injected": true}.
    let mut registry = HookRegistry::new();
    registry.register(Box::new(InjectArgsHook));

    let capture = std::sync::Arc::new(CaptureArgsBeforeHook::new());
    struct ArcCaptureBefore(std::sync::Arc<CaptureArgsBeforeHook>);
    impl AgentHook for ArcCaptureBefore {
        fn name(&self) -> &'static str {
            self.0.name()
        }
        fn point(&self) -> agent::HookPoint {
            self.0.point()
        }
        fn should_run(&self, a: &[ChatMessage], b: config::Provider, c: usize, d: usize) -> bool {
            self.0.should_run(a, b, c, d)
        }
        fn run<'a>(
            &'a self,
            i: HookInput,
            c: &'a Config,
            h: &'a reqwest::Client,
        ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
            self.0.run(i, c, h)
        }
        fn should_run_tool(&self, n: &str, p: agent::HookPoint) -> bool {
            self.0.should_run_tool(n, p)
        }
        fn run_tool<'a>(
            &'a self,
            i: ToolHookInput,
            c: &'a Config,
        ) -> Pin<Box<dyn Future<Output = HookOutput> + Send + 'a>> {
            self.0.run_tool(i, c)
        }
    }
    registry.register(Box::new(ArcCaptureBefore(capture.clone())));

    let config = test_config();
    let input = test_tool_input("some_tool", serde_json::json!({"key": "val"}));
    let output = run_tool_hooks(&registry, agent::HookPoint::BeforeToolExec, input, &config).await;

    // Final output should include both hooks' modifications.
    match &output {
        HookOutput::ModifyToolArgs { args } => {
            assert_eq!(args["key"], "val");
            assert_eq!(args["injected"], true, "hook A's injection");
            assert_eq!(args["captured_phase"], true, "hook B's injection");
        }
        _ => panic!("expected ModifyToolArgs"),
    }

    // Hook B should have seen hook A's modification.
    let seen = capture.captured().expect("hook B should have fired");
    assert_eq!(
        seen["injected"], true,
        "hook B must see hook A's modification"
    );
    assert_eq!(seen["key"], "val");
}

// ── Tests: re-prune integration (hook extra_system → over budget → prune) ────

#[tokio::test]
async fn reprune_integration_hook_extra_system_triggers_prune_and_fits_budget() {
    // Simulates the exact re-prune sequence in run_analyze_phase:
    // 1. Build messages snapshot
    // 2. Hook adds large extra_system to system message
    // 3. Message-level estimate exceeds message budget
    // 4. Re-prune trims messages to fit message budget
    // 5. Final estimate fits
    use crate::config::Provider;
    use crate::context::{
        estimate_tokens_for_provider, prune_messages_for_provider,
        request_message_budget_for_runtime,
    };

    let config = test_config();
    let provider = Provider::OpenAI;
    let extra_tools: Vec<serde_json::Value> = Vec::new();

    // Compute the real message budget the runtime would use.
    let message_budget =
        request_message_budget_for_runtime(&config, &config.model, "none", &extra_tools);

    // Start with a conversation that's just below the message budget.
    let mut messages = vec![ChatMessage {
        role: "system".to_string(),
        content: Some("You are a helpful assistant.".to_string()),
        images: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    // Fill with turns until close to message budget.
    let padding = "a]b[c".repeat(40); // ~200 chars per turn for faster filling
    for i in 0..2000 {
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(format!("Q{i}: {padding}")),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        });
        messages.push(ChatMessage {
            role: "assistant".to_string(),
            content: Some(format!("A{i}: {padding}")),
            images: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        });
        let est = estimate_tokens_for_provider(provider, &messages);
        if est > message_budget.saturating_sub(500) {
            break;
        }
    }

    let pre_hook = estimate_tokens_for_provider(provider, &messages);
    assert!(
        pre_hook <= message_budget,
        "pre-hook ({pre_hook}) should fit in message budget ({message_budget})"
    );

    // Inject extra_system that pushes over budget (simulates BeforeLlmCall hook).
    let extra_system = "CRITICAL OVERRIDE: ".to_string() + &"x".repeat(4000);
    if let Some(first) = messages.first_mut()
        && first.role == "system"
        && let Some(content) = first.content.as_mut()
    {
        content.push('\n');
        content.push_str(&extra_system);
    }

    let post_hook = estimate_tokens_for_provider(provider, &messages);
    assert!(
        post_hook > message_budget,
        "after hook injection ({post_hook}), should exceed message budget ({message_budget})"
    );

    // Re-prune (mirrors runtime_loop.rs:872-882).
    prune_messages_for_provider(&mut messages, provider, message_budget);

    let final_est = estimate_tokens_for_provider(provider, &messages);
    assert!(
        final_est <= message_budget,
        "after re-prune, estimate ({final_est}) must fit message budget ({message_budget})"
    );
    // System message (with extra_system) must survive.
    assert_eq!(messages[0].role, "system");
    assert!(
        messages[0]
            .content
            .as_ref()
            .unwrap()
            .contains("CRITICAL OVERRIDE"),
        "extra_system content must survive re-prune"
    );
    assert!(messages.len() >= 2, "at least system + one message");
}
