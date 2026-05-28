pub(crate) mod exec;
pub(crate) mod fs;
pub(crate) mod mcp;
pub(crate) mod net;

use reqwest::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    path::Path,
    pin::Pin,
    time::{Duration, Instant},
};
use tokio::sync::mpsc::{Receiver, Sender, UnboundedSender};

use crate::Config;

/// Structured tool execution result with metadata.
pub(crate) struct ToolOutcome {
    pub output: String,
    pub is_error: bool,
    pub duration_ms: u64,
    pub subagent_snapshot: Option<crate::SubagentHistorySnapshot>,
}

pub(super) struct ToolHandlerOutput {
    output: String,
    is_error: Option<bool>,
}

#[derive(Clone, Debug)]
pub(crate) enum ToolLiveEvent {
    ExecOutput { stream: &'static str, chunk: String },
}

pub(crate) type ToolEventSender = UnboundedSender<ToolLiveEvent>;
pub(crate) type BoundedToolEventSender = Sender<ToolLiveEvent>;
pub(crate) type BoundedToolEventReceiver = Receiver<ToolLiveEvent>;
pub(crate) const TOOL_LIVE_EVENT_CHANNEL_CAPACITY: usize = 256;

pub(crate) async fn forward_exec_live_event<F, Fut>(event: ToolLiveEvent, on_event: &mut F)
where
    F: FnMut(&'static str, String) -> Fut,
    Fut: Future<Output = ()>,
{
    let ToolLiveEvent::ExecOutput { stream, chunk } = event;
    on_event(stream, chunk).await;
}

pub(crate) async fn drain_bounded_exec_live_events<F, Fut>(
    event_rx: &mut BoundedToolEventReceiver,
    on_event: &mut F,
) where
    F: FnMut(&'static str, String) -> Fut,
    Fut: Future<Output = ()>,
{
    while let Ok(event) = event_rx.try_recv() {
        forward_exec_live_event(event, on_event).await;
    }
}

impl ToolHandlerOutput {
    fn inferred(output: String) -> Self {
        Self {
            output,
            is_error: None,
        }
    }

    fn explicit(output: String, is_error: bool) -> Self {
        Self {
            output,
            is_error: Some(is_error),
        }
    }
}

type ToolFuture<'a> = Pin<Box<dyn Future<Output = ToolHandlerOutput> + Send + 'a>>;
type ToolHandler = for<'a> fn(
    &'a serde_json::Value,
    &'a Config,
    &'a Client,
    &'a Path,
    Option<ToolEventSender>,
    Option<BoundedToolEventSender>,
) -> ToolFuture<'a>;
type ToolTraceBuilder = fn(&serde_json::Value) -> Option<crate::agent::ToolExecutionTrace>;

pub(crate) const TOOL_NAME_THINK: &str = "think";
pub(crate) const TOOL_NAME_TODOS: &str = "todos";
pub(crate) const TOOL_NAME_EXEC: &str = "exec";
pub(crate) const TOOL_NAME_READ_FILE: &str = "read_file";
pub(crate) const TOOL_NAME_WRITE_FILE: &str = "write_file";
pub(crate) const TOOL_NAME_PATCH_FILE: &str = "patch_file";
pub(crate) const TOOL_NAME_LIST_DIR: &str = "list_dir";
pub(crate) const TOOL_NAME_SEARCH_FILES: &str = "search_files";
pub(crate) const TOOL_NAME_HTTP_FETCH: &str = "http_fetch";
pub(crate) const TOOL_NAME_DELETE_FILE: &str = "delete_file";
pub(crate) const TOOL_NAME_TASK: &str = "task";
pub(crate) const TOOL_NAME_ORCHESTRATE: &str = "orchestrate";

pub(crate) fn tool_runtime_timeout(tool_name: &str, config: &Config) -> Option<Duration> {
    if matches!(tool_name, TOOL_NAME_EXEC | TOOL_NAME_TODOS) {
        None
    } else {
        Some(config.tool_timeout)
    }
}

pub(crate) struct ToolSpec {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    relevance_hint: &'static str,
    prompt_line: fn(&Config) -> String,
    pub(crate) parameters: fn() -> serde_json::Value,
    handler: ToolHandler,
    trace_builder: ToolTraceBuilder,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ToolRankingContext {
    pub preferred_tools: Vec<String>,
}

const TOOL_FULL_DISPLAY_THRESHOLD: usize = 6;
const TOOL_TOP_N: usize = 5;
const TOOL_PREFERENCE_BOOST: usize = 4;

fn tool_parameters_think() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "thought": {
                "type": "string",
                "minLength": 1,
                "maxLength": 20000,
                "description": "Your step-by-step reasoning and plan"
            }
        },
        "required": ["thought"]
    })
}

fn tool_parameters_todos() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "base_revision": {
                "type": "integer",
                "minimum": 0,
                "description": "Current todo revision expected by the caller"
            },
            "items": {
                "type": "array",
                "maxItems": crate::todos::MAX_TODO_ITEMS,
                "description": "Full ordered todo list replacement",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": crate::todos::MAX_TODO_ID_CHARS
                        },
                        "content": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": crate::todos::MAX_TODO_CONTENT_CHARS
                        },
                        "status": {
                            "type": "string",
                            "enum": ["pending", "in_progress", "completed"]
                        }
                    },
                    "required": ["id", "content", "status"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["base_revision", "items"],
        "additionalProperties": false
    })
}

fn tool_parameters_exec() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "minLength": 1,
                "maxLength": 20000,
                "description": "Shell command to execute. Prefer 'program' + 'args' when a shell is not required."
            },
            "program": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "description": "Executable to run directly without a shell"
            },
            "args": {
                "type": "array",
                "maxItems": 256,
                "description": "Argument vector for direct execution mode",
                "items": {
                    "type": "string",
                    "maxLength": 20000
                }
            },
            "working_dir": {
                "type": "string",
                "maxLength": 4096,
                "description": "Working directory (default: workspace root)"
            },
            "env": {
                "type": "object",
                "description": "Additional environment variables to set for the process",
                "additionalProperties": {
                    "type": "string",
                    "maxLength": 20000
                }
            },
            "stdin": {
                "type": "string",
                "maxLength": 200000,
                "description": "Text to pipe to the process standard input"
            }
        },
        "required": [],
        "additionalProperties": false
    })
}

fn tool_parameters_read_file() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "description": "File path to read inside the session workspace"
            },
            "start_line": {
                "type": "integer",
                "minimum": 1,
                "maximum": 1000000,
                "description": "Starting line number (1-based, optional)"
            },
            "end_line": {
                "type": "integer",
                "minimum": 1,
                "maximum": 1000000,
                "description": "Ending line number (inclusive, optional)"
            }
        },
        "required": ["path"]
    })
}

fn tool_parameters_write_file() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "description": "File path to write inside the session workspace"
            },
            "content": {
                "type": "string",
                "maxLength": 1000000,
                "description": "Content to write to the file"
            }
        },
        "required": ["path", "content"]
    })
}

fn tool_parameters_patch_file() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "description": "File path to patch inside the session workspace"
            },
            "old_string": {
                "type": "string",
                "minLength": 1,
                "maxLength": 1000000,
                "description": "Exact string to find (must exist in the file)"
            },
            "new_string": {
                "type": "string",
                "maxLength": 1000000,
                "description": "Replacement string"
            }
        },
        "required": ["path", "old_string", "new_string"]
    })
}

fn tool_parameters_list_dir() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "maxLength": 4096,
                "description": "Directory path inside the session workspace (default: workspace root)"
            }
        },
        "required": []
    })
}

fn tool_parameters_search_files() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "pattern": {
                "type": "string",
                "minLength": 1,
                "maxLength": 2000,
                "description": "Regex pattern to search for"
            },
            "path": {
                "type": "string",
                "maxLength": 4096,
                "description": "Directory to search in inside the session workspace (default: workspace root)"
            },
            "file_glob": {
                "type": "string",
                "maxLength": 256,
                "description": "File name filter, e.g. '*.rs' (default: all files)"
            },
            "max_results": {
                "type": "integer",
                "minimum": 1,
                "maximum": 200,
                "description": "Maximum number of results (default: 50)"
            }
        },
        "required": ["pattern"]
    })
}

fn tool_parameters_http_fetch() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "url": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "description": "URL to fetch"
            },
            "max_bytes": {
                "type": "integer",
                "minimum": 1,
                "maximum": 1000000,
                "description": "Maximum response size in bytes (default: 102400)"
            }
        },
        "required": ["url"]
    })
}

fn tool_parameters_delete_file() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "minLength": 1,
                "maxLength": 4096,
                "description": "File path to delete inside the session workspace"
            }
        },
        "required": ["path"]
    })
}

fn tool_prompt_line_think(_: &Config) -> String {
    "**think** — Plan your approach step-by-step before complex tasks. Write reasoning here."
        .to_string()
}

fn tool_prompt_line_todos(_: &Config) -> String {
    "**todos** — Replace the session todo list with an ordered checklist for multi-step work. Use this to track progress and react to user edits."
        .to_string()
}

fn tool_prompt_line_exec(config: &Config) -> String {
    format!(
        "**exec** — Execute commands (timeout: {}s). Prefer `program` + `args`; use `command` for shell-only workflows. Supports working_dir, env, stdin.",
        config.exec_timeout.as_secs()
    )
}

fn tool_prompt_line_read_file(_: &Config) -> String {
    "**read_file** — Read file contents from the session workspace. Supports line range (start_line, end_line).".to_string()
}

fn tool_prompt_line_write_file(_: &Config) -> String {
    "**write_file** — Create or overwrite files inside the session workspace.".to_string()
}

fn tool_prompt_line_patch_file(_: &Config) -> String {
    "**patch_file** — Find and replace exact strings in session workspace files.".to_string()
}

fn tool_prompt_line_list_dir(_: &Config) -> String {
    "**list_dir** — List session workspace directory contents with file sizes.".to_string()
}

fn tool_prompt_line_search_files(_: &Config) -> String {
    "**search_files** — Regex search across files in the session workspace (like grep).".to_string()
}

fn tool_prompt_line_http_fetch(_: &Config) -> String {
    "**http_fetch** — Fetch content from a URL via HTTP GET.".to_string()
}

fn tool_prompt_line_delete_file(_: &Config) -> String {
    "**delete_file** — Delete a file from the session workspace.".to_string()
}

fn tool_handler_think<'a>(
    args: &'a serde_json::Value,
    _: &'a Config,
    _: &'a Client,
    _: &'a Path,
    _: Option<ToolEventSender>,
    _: Option<BoundedToolEventSender>,
) -> ToolFuture<'a> {
    Box::pin(async move { ToolHandlerOutput::explicit(exec::tool_think(args), false) })
}

fn tool_handler_todos<'a>(
    _: &'a serde_json::Value,
    _: &'a Config,
    _: &'a Client,
    _: &'a Path,
    _: Option<ToolEventSender>,
    _: Option<BoundedToolEventSender>,
) -> ToolFuture<'a> {
    Box::pin(async move {
        ToolHandlerOutput::explicit(
            "todos error: runtime session context unavailable".to_string(),
            true,
        )
    })
}

fn tool_handler_exec<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
    event_tx: Option<ToolEventSender>,
    bounded_event_tx: Option<BoundedToolEventSender>,
) -> ToolFuture<'a> {
    Box::pin(
        async move { exec::tool_exec(args, config, workspace, event_tx, bounded_event_tx).await },
    )
}

fn tool_handler_read_file<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
    _: Option<ToolEventSender>,
    _: Option<BoundedToolEventSender>,
) -> ToolFuture<'a> {
    Box::pin(async move {
        ToolHandlerOutput::inferred(fs::tool_read_file(args, config, workspace).await)
    })
}

fn tool_handler_write_file<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
    _: Option<ToolEventSender>,
    _: Option<BoundedToolEventSender>,
) -> ToolFuture<'a> {
    Box::pin(async move {
        ToolHandlerOutput::inferred(fs::tool_write_file(args, config, workspace).await)
    })
}

fn tool_handler_patch_file<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
    _: Option<ToolEventSender>,
    _: Option<BoundedToolEventSender>,
) -> ToolFuture<'a> {
    Box::pin(async move {
        ToolHandlerOutput::inferred(fs::tool_patch_file(args, config, workspace).await)
    })
}

fn tool_handler_list_dir<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
    _: Option<ToolEventSender>,
    _: Option<BoundedToolEventSender>,
) -> ToolFuture<'a> {
    Box::pin(async move {
        ToolHandlerOutput::inferred(fs::tool_list_dir(args, config, workspace).await)
    })
}

fn tool_handler_search_files<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
    _: Option<ToolEventSender>,
    _: Option<BoundedToolEventSender>,
) -> ToolFuture<'a> {
    Box::pin(async move {
        ToolHandlerOutput::inferred(fs::tool_search_files(args, config, workspace).await)
    })
}

fn tool_handler_http_fetch<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    http: &'a Client,
    _: &'a Path,
    _: Option<ToolEventSender>,
    _: Option<BoundedToolEventSender>,
) -> ToolFuture<'a> {
    Box::pin(
        async move { ToolHandlerOutput::inferred(net::tool_http_fetch(args, http, config).await) },
    )
}

fn tool_handler_delete_file<'a>(
    args: &'a serde_json::Value,
    _: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
    _: Option<ToolEventSender>,
    _: Option<BoundedToolEventSender>,
) -> ToolFuture<'a> {
    Box::pin(
        async move { ToolHandlerOutput::inferred(fs::tool_delete_file(args, workspace).await) },
    )
}

fn trace_builder_none(_: &serde_json::Value) -> Option<crate::agent::ToolExecutionTrace> {
    None
}

fn trace_builder_todos(args: &serde_json::Value) -> Option<crate::agent::ToolExecutionTrace> {
    let item_count = args
        .get("items")
        .and_then(serde_json::Value::as_array)
        .map(std::vec::Vec::len)?;
    let base_revision = tool_arg_u64(args, "base_revision")?;
    Some(crate::agent::ToolExecutionTrace {
        summary: compact_tool_call_summary(&format!(
            "replace todos with {item_count} items at revision {base_revision}"
        )),
        ..crate::agent::ToolExecutionTrace::default()
    })
}

fn trace_builder_exec(args: &serde_json::Value) -> Option<crate::agent::ToolExecutionTrace> {
    let command = exec::summarize_exec_request(args)?;
    let working_dir = tool_arg_str(args, "working_dir")
        .filter(|dir| !dir.is_empty() && *dir != ".")
        .map(str::to_string);
    let summary = match working_dir.as_deref() {
        Some(dir) => format!("run `{command}` in `{dir}`"),
        None => format!("run `{command}`"),
    };
    Some(crate::agent::ToolExecutionTrace {
        summary: compact_tool_call_summary(&summary),
        command: Some(command),
        working_dir,
        ..crate::agent::ToolExecutionTrace::default()
    })
}

fn trace_builder_read_file(args: &serde_json::Value) -> Option<crate::agent::ToolExecutionTrace> {
    let path = tool_arg_str(args, "path")?;
    let lines = format_line_window(args);
    let summary = match lines {
        Some(lines) => format!("read `{path}` {lines}"),
        None => format!("read `{path}`"),
    };
    Some(crate::agent::ToolExecutionTrace {
        summary: compact_tool_call_summary(&summary),
        path: Some(path.to_string()),
        start_line: tool_arg_u64(args, "start_line").map(|value| value as usize),
        end_line: tool_arg_u64(args, "end_line").map(|value| value as usize),
        ..crate::agent::ToolExecutionTrace::default()
    })
}

fn trace_builder_write_file(args: &serde_json::Value) -> Option<crate::agent::ToolExecutionTrace> {
    let path = tool_arg_str(args, "path")?;
    Some(crate::agent::ToolExecutionTrace {
        summary: compact_tool_call_summary(&format!("write `{path}`")),
        path: Some(path.to_string()),
        ..crate::agent::ToolExecutionTrace::default()
    })
}

fn trace_builder_patch_file(args: &serde_json::Value) -> Option<crate::agent::ToolExecutionTrace> {
    let path = tool_arg_str(args, "path")?;
    Some(crate::agent::ToolExecutionTrace {
        summary: compact_tool_call_summary(&format!("patch `{path}`")),
        path: Some(path.to_string()),
        ..crate::agent::ToolExecutionTrace::default()
    })
}

fn trace_builder_delete_file(args: &serde_json::Value) -> Option<crate::agent::ToolExecutionTrace> {
    let path = tool_arg_str(args, "path")?;
    Some(crate::agent::ToolExecutionTrace {
        summary: compact_tool_call_summary(&format!("delete `{path}`")),
        path: Some(path.to_string()),
        ..crate::agent::ToolExecutionTrace::default()
    })
}

fn trace_builder_list_dir(args: &serde_json::Value) -> Option<crate::agent::ToolExecutionTrace> {
    let path = tool_arg_str(args, "path").unwrap_or(".");
    Some(crate::agent::ToolExecutionTrace {
        summary: compact_tool_call_summary(&format!("list `{path}`")),
        path: Some(path.to_string()),
        ..crate::agent::ToolExecutionTrace::default()
    })
}

fn trace_builder_search_files(
    args: &serde_json::Value,
) -> Option<crate::agent::ToolExecutionTrace> {
    let pattern = tool_arg_str(args, "pattern")?;
    let scope = tool_arg_str(args, "path").unwrap_or(".");
    let glob = tool_arg_str(args, "file_glob");
    let summary = match glob {
        Some(glob) => format!("search `{pattern}` in `{scope}` with `{glob}`"),
        None => format!("search `{pattern}` in `{scope}`"),
    };
    Some(crate::agent::ToolExecutionTrace {
        summary: compact_tool_call_summary(&summary),
        path: Some(scope.to_string()),
        pattern: Some(pattern.to_string()),
        file_glob: glob.map(str::to_string),
        ..crate::agent::ToolExecutionTrace::default()
    })
}

fn trace_builder_http_fetch(args: &serde_json::Value) -> Option<crate::agent::ToolExecutionTrace> {
    let url = tool_arg_str(args, "url")?;
    Some(crate::agent::ToolExecutionTrace {
        summary: compact_tool_call_summary(&format!("fetch `{url}`")),
        url: Some(url.to_string()),
        ..crate::agent::ToolExecutionTrace::default()
    })
}

fn trace_builder_task(args: &serde_json::Value) -> Option<crate::agent::ToolExecutionTrace> {
    let agent_name = tool_arg_str(args, "agent")?;
    let retry_key = tool_arg_str(args, "prompt").map(|prompt| {
        trace_retry_key_from_str(
            "task",
            &format!("{agent_name}\u{1f}|{}", normalize_retry_text(prompt)),
        )
    });
    Some(crate::agent::ToolExecutionTrace {
        summary: compact_tool_call_summary(&format!("delegate to `{agent_name}`")),
        agent: Some(agent_name.to_string()),
        retry_key,
        ..crate::agent::ToolExecutionTrace::default()
    })
}

fn trace_builder_orchestrate(args: &serde_json::Value) -> Option<crate::agent::ToolExecutionTrace> {
    let tasks = args.get("tasks")?;
    let task_count = args
        .get("tasks")
        .and_then(serde_json::Value::as_array)
        .map(std::vec::Vec::len)?;
    Some(crate::agent::ToolExecutionTrace {
        summary: compact_tool_call_summary(&format!("orchestrate {task_count} delegated tasks")),
        task_count: Some(task_count),
        retry_key: trace_retry_key_from_value("orchestrate", tasks),
        ..crate::agent::ToolExecutionTrace::default()
    })
}

fn normalize_retry_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn trace_retry_key_from_value(prefix: &str, value: &serde_json::Value) -> Option<String> {
    serde_json::to_string(value)
        .ok()
        .map(|json| trace_retry_key_from_str(prefix, &json))
}

fn trace_retry_key_from_str(prefix: &str, value: &str) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(value.as_bytes());
    let mut hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    format!("{prefix}:{hex}")
}

pub(crate) fn tool_specs() -> &'static [ToolSpec] {
    &[
        ToolSpec {
            name: TOOL_NAME_THINK,
            description: "Plan your approach step by step before acting on complex tasks. Use this to organize your thoughts before a series of tool calls.",
            relevance_hint: "plan outline strategy reasoning steps analyze",
            prompt_line: tool_prompt_line_think,
            parameters: tool_parameters_think,
            handler: tool_handler_think,
            trace_builder: trace_builder_none,
        },
        ToolSpec {
            name: TOOL_NAME_TODOS,
            description: "Replace the session todo list with an ordered checklist snapshot. Use this for multi-step plans, progress tracking, and adapting to user edits.",
            relevance_hint: "todo checklist tasks plan progress tracking ordered steps",
            prompt_line: tool_prompt_line_todos,
            parameters: tool_parameters_todos,
            handler: tool_handler_todos,
            trace_builder: trace_builder_todos,
        },
        ToolSpec {
            name: TOOL_NAME_EXEC,
            description: "Execute a command and return stdout + stderr. Prefer direct program + args execution when a shell is not required; use shell command mode for pipelines and shell builtins.",
            relevance_hint: "run shell command program args build test git benchmark profile compile install",
            prompt_line: tool_prompt_line_exec,
            parameters: tool_parameters_exec,
            handler: tool_handler_exec,
            trace_builder: trace_builder_exec,
        },
        ToolSpec {
            name: TOOL_NAME_READ_FILE,
            description: "Read a file's contents. Supports optional line range for large files.",
            relevance_hint: "read inspect open cat file source code contents lines",
            prompt_line: tool_prompt_line_read_file,
            parameters: tool_parameters_read_file,
            handler: tool_handler_read_file,
            trace_builder: trace_builder_read_file,
        },
        ToolSpec {
            name: TOOL_NAME_WRITE_FILE,
            description: "Create a new file or overwrite an existing file with the given content.",
            relevance_hint: "create write save generate file content",
            prompt_line: tool_prompt_line_write_file,
            parameters: tool_parameters_write_file,
            handler: tool_handler_write_file,
            trace_builder: trace_builder_write_file,
        },
        ToolSpec {
            name: TOOL_NAME_PATCH_FILE,
            description: "Find and replace a specific string in a file. The old_string must match exactly.",
            relevance_hint: "edit modify update patch replace refactor fix file",
            prompt_line: tool_prompt_line_patch_file,
            parameters: tool_parameters_patch_file,
            handler: tool_handler_patch_file,
            trace_builder: trace_builder_patch_file,
        },
        ToolSpec {
            name: TOOL_NAME_LIST_DIR,
            description: "List the contents of a directory with file type and size information.",
            relevance_hint: "directory folder tree files structure browse workspace",
            prompt_line: tool_prompt_line_list_dir,
            parameters: tool_parameters_list_dir,
            handler: tool_handler_list_dir,
            trace_builder: trace_builder_list_dir,
        },
        ToolSpec {
            name: TOOL_NAME_SEARCH_FILES,
            description: "Search for a regex pattern in files. Returns matching lines with file paths and line numbers, like grep.",
            relevance_hint: "search grep rg find pattern references symbols codebase",
            prompt_line: tool_prompt_line_search_files,
            parameters: tool_parameters_search_files,
            handler: tool_handler_search_files,
            trace_builder: trace_builder_search_files,
        },
        ToolSpec {
            name: TOOL_NAME_HTTP_FETCH,
            description: "Fetch content from a URL using HTTP GET. Returns status code and response body.",
            relevance_hint: "fetch request url api docs website http",
            prompt_line: tool_prompt_line_http_fetch,
            parameters: tool_parameters_http_fetch,
            handler: tool_handler_http_fetch,
            trace_builder: trace_builder_http_fetch,
        },
        ToolSpec {
            name: TOOL_NAME_DELETE_FILE,
            description: "Delete a file from the workspace. The path must be inside the session workspace.",
            relevance_hint: "delete remove cleanup file",
            prompt_line: tool_prompt_line_delete_file,
            parameters: tool_parameters_delete_file,
            handler: tool_handler_delete_file,
            trace_builder: trace_builder_delete_file,
        },
    ]
}

fn find_tool_spec(name: &str) -> Option<&'static ToolSpec> {
    tool_specs().iter().find(|spec| spec.name == name)
}

pub(crate) fn build_tool_execution_trace(
    tool_name: &str,
    effective_args: Option<&str>,
) -> Option<crate::agent::ToolExecutionTrace> {
    let args = effective_args
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .unwrap_or(serde_json::Value::Null);

    if let Some(spec) = find_tool_spec(tool_name) {
        return (spec.trace_builder)(&args);
    }

    match tool_name {
        TOOL_NAME_TASK => trace_builder_task(&args),
        TOOL_NAME_ORCHESTRATE => trace_builder_orchestrate(&args),
        _ => None,
    }
}

pub(crate) fn display_tool_arguments(tool_name: &str, raw_args: &str) -> String {
    if tool_name != TOOL_NAME_EXEC {
        return raw_args.to_string();
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw_args) else {
        return exec::sanitize_exec_command_for_display(raw_args);
    };
    serde_json::to_string(&sanitize_exec_argument_value(None, &value))
        .unwrap_or_else(|_| exec::sanitize_exec_command_for_display(raw_args))
}

pub(crate) fn sanitize_chat_message_tool_calls_in_place(message: &mut crate::ChatMessage) {
    if let Some(tool_calls) = message.tool_calls.as_mut() {
        for tool_call in tool_calls {
            tool_call.function.arguments =
                display_tool_arguments(&tool_call.function.name, &tool_call.function.arguments);
        }
    }
}

pub(crate) fn sanitize_subagent_snapshot_tool_args_in_place(
    snapshot: &mut crate::SubagentHistorySnapshot,
) {
    for tool in &mut snapshot.tools {
        if let Some(arguments) = tool.arguments.as_deref() {
            tool.arguments = Some(display_tool_arguments(&tool.name, arguments));
        }
    }
}

fn sanitize_exec_argument_value(
    key_hint: Option<&str>,
    value: &serde_json::Value,
) -> serde_json::Value {
    if key_hint.is_some_and(is_exec_secret_field_name) {
        return serde_json::Value::String(exec::REDACTED_VALUE.to_string());
    }

    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), sanitize_exec_argument_value(Some(key), value)))
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(sanitize_exec_argument_array(values))
        }
        serde_json::Value::String(text) => {
            let sanitized = exec::sanitize_exec_command_for_display(text);
            let rendered = if sanitized == text.as_str() && looks_like_bare_secret(text) {
                exec::REDACTED_VALUE.to_string()
            } else {
                sanitized
            };
            serde_json::Value::String(rendered)
        }
        _ => value.clone(),
    }
}

fn sanitize_exec_argument_array(values: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut sanitized = Vec::with_capacity(values.len());
    let mut redact_next_string = false;

    for value in values {
        let sanitized_value = if redact_next_string {
            value
                .as_str()
                .map(|_| serde_json::Value::String(exec::REDACTED_VALUE.to_string()))
                .unwrap_or_else(|| sanitize_exec_argument_value(None, value))
        } else {
            sanitize_exec_argument_value(None, value)
        };

        redact_next_string = value.as_str().is_some_and(is_exec_secret_flag_token);
        sanitized.push(sanitized_value);
    }

    sanitized
}

fn is_exec_secret_field_name(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect();
    normalized == "authorization"
        || normalized == "apikey"
        || normalized.ends_with("apikey")
        || normalized == "token"
        || normalized.ends_with("token")
        || normalized == "accesstoken"
        || normalized.ends_with("accesstoken")
        || normalized == "password"
        || normalized.ends_with("password")
        || normalized == "passwd"
        || normalized.ends_with("passwd")
        || normalized == "secret"
        || normalized.ends_with("secret")
}

fn is_exec_secret_flag_token(token: &str) -> bool {
    let trimmed = token.trim();
    let Some(flag) = trimmed
        .strip_prefix("--")
        .or_else(|| trimmed.strip_prefix('-'))
    else {
        return false;
    };

    if flag.contains('=') {
        return false;
    }

    is_exec_secret_field_name(flag)
}

fn looks_like_bare_secret(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return false;
    }

    let lower = trimmed.to_ascii_lowercase();
    ["sk-", "ghp_", "github_pat_", "hf_", "xox", "ya29.", "eyj"]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

fn tool_arg_str<'a>(args: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    args.get(key)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn tool_arg_u64(args: &serde_json::Value, key: &str) -> Option<u64> {
    args.get(key)?.as_u64()
}

fn format_line_window(args: &serde_json::Value) -> Option<String> {
    let start = args.get("start_line").and_then(serde_json::Value::as_u64);
    let end = args.get("end_line").and_then(serde_json::Value::as_u64);
    match (start, end) {
        (Some(start), Some(end)) => Some(format!("lines {start}-{end}")),
        (Some(start), None) => Some(format!("from line {start}")),
        (None, Some(end)) => Some(format!("through line {end}")),
        (None, None) => None,
    }
}

fn compact_tool_call_summary(text: &str) -> String {
    crate::truncate(&text.split_whitespace().collect::<Vec<_>>().join(" "), 180)
}

#[allow(dead_code)] // Compatibility wrapper for call sites that still want the full tool list.
pub(crate) fn render_tool_prompt_lines(config: &Config) -> String {
    render_tool_prompt_lines_with_query(config, None)
}

pub(crate) fn render_tool_prompt_lines_with_query(
    config: &Config,
    current_query: Option<&str>,
) -> String {
    let specs = tool_specs();
    let prompt_lines: Vec<String> = specs
        .iter()
        .map(|spec| (spec.prompt_line)(config))
        .collect();

    if let Some(selected) = select_ranked_tool_indices(&specs, &prompt_lines, current_query, None) {
        let mut display_order = selected.clone();
        display_order.sort_unstable();

        let mut lines = Vec::new();
        for (display_idx, idx) in display_order.iter().enumerate() {
            lines.push(format!("{}. {}", display_idx + 1, prompt_lines[*idx]));
        }

        let remaining: Vec<&str> = specs
            .iter()
            .enumerate()
            .filter(|(idx, _)| !display_order.contains(idx))
            .map(|(_, spec)| spec.name)
            .collect();
        if !remaining.is_empty() {
            lines.push(format!(
                "Other available tools: {}",
                remaining
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        return lines.join("\n");
    }

    prompt_lines
        .into_iter()
        .enumerate()
        .map(|(idx, line)| format!("{}. {line}", idx + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn render_ranked_tool_recommendations(
    config: &Config,
    current_query: Option<&str>,
    ranking: &ToolRankingContext,
) -> Option<String> {
    let specs = tool_specs();
    let prompt_lines: Vec<String> = specs
        .iter()
        .map(|spec| (spec.prompt_line)(config))
        .collect();
    let selected = select_ranked_tool_indices(&specs, &prompt_lines, current_query, Some(ranking))?;

    let mut lines = vec!["## Suggested Tool Order".to_string()];
    for (display_idx, idx) in selected.iter().enumerate() {
        lines.push(format!("{}. {}", display_idx + 1, prompt_lines[*idx]));
    }
    Some(lines.join("\n"))
}

fn select_ranked_tool_indices(
    specs: &[ToolSpec],
    prompt_lines: &[String],
    current_query: Option<&str>,
    ranking: Option<&ToolRankingContext>,
) -> Option<Vec<usize>> {
    if specs.len() <= TOOL_FULL_DISPLAY_THRESHOLD {
        return None;
    }

    let query_tokens = current_query
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(crate::tokenize_for_matching)
        .unwrap_or_default();
    let mut ranked: Vec<(usize, usize)> = specs
        .iter()
        .enumerate()
        .map(|(idx, spec)| {
            (
                tool_relevance(spec, &prompt_lines[idx], &query_tokens)
                    + tool_preference_boost(spec.name, ranking),
                idx,
            )
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));

    if ranked.first().map(|(score, _)| *score).unwrap_or(0) == 0 {
        return None;
    }

    let ranked_indices: Vec<usize> = ranked.iter().map(|(_, idx)| *idx).collect();
    let mut selected: Vec<usize> = ranked
        .iter()
        .take(TOOL_TOP_N)
        .map(|(_, idx)| *idx)
        .collect();
    ensure_think_tool(specs, &ranked_indices, &mut selected);
    Some(selected)
}

fn ensure_think_tool(specs: &[ToolSpec], ranked_indices: &[usize], selected: &mut Vec<usize>) {
    if selected
        .iter()
        .any(|idx| specs[*idx].name == TOOL_NAME_THINK)
    {
        return;
    }
    if let Some(think_idx) = specs.iter().position(|spec| spec.name == TOOL_NAME_THINK) {
        if selected.len() >= TOOL_TOP_N {
            selected.pop();
        }
        let insert_at = ranked_indices
            .iter()
            .position(|idx| *idx == think_idx)
            .and_then(|think_rank| {
                selected.iter().position(|idx| {
                    ranked_indices
                        .iter()
                        .position(|candidate| candidate == idx)
                        .is_some_and(|rank| rank > think_rank)
                })
            })
            .unwrap_or(selected.len());
        selected.insert(insert_at, think_idx);
    }
}

fn tool_relevance(spec: &ToolSpec, prompt_line: &str, query_tokens: &[String]) -> usize {
    if query_tokens.is_empty() {
        return 0;
    }
    let text = format!(
        "{} {} {} {}",
        spec.name, spec.description, prompt_line, spec.relevance_hint
    )
    .to_lowercase();

    query_tokens
        .iter()
        .filter(|token| !token.is_empty() && text.contains(token.as_str()))
        .count()
}

fn tool_preference_boost(name: &str, ranking: Option<&ToolRankingContext>) -> usize {
    ranking
        .map(|ranking| {
            ranking
                .preferred_tools
                .iter()
                .filter(|preferred| preferred.eq_ignore_ascii_case(name))
                .count()
                * TOOL_PREFERENCE_BOOST
        })
        .unwrap_or(0)
}

pub(crate) fn tool_definitions() -> serde_json::Value {
    tool_definitions_openai()
}

pub(crate) fn tool_definitions_openai() -> serde_json::Value {
    let tools = tool_specs()
        .iter()
        .map(|spec| {
            json!({
                "type": "function",
                "function": {
                    "name": spec.name,
                    "description": spec.description,
                    "parameters": (spec.parameters)(),
                }
            })
        })
        .collect::<Vec<_>>();
    json!(tools)
}

pub(crate) fn tool_definitions_ollama() -> serde_json::Value {
    tool_definitions_openai()
}

pub(crate) fn tool_definitions_gemini() -> serde_json::Value {
    let tools = tool_specs()
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "parameters": gemini_tool_parameters((spec.parameters)()),
            })
        })
        .collect::<Vec<_>>();
    json!(tools)
}

pub(crate) fn gemini_tool_parameters(parameters: Value) -> Value {
    let normalized = normalize_gemini_schema(parameters);
    if normalized.is_object() {
        normalized
    } else {
        json!({ "type": "object" })
    }
}

fn normalize_gemini_schema(value: Value) -> Value {
    let Value::Object(input) = value else {
        return value;
    };

    let mut output = serde_json::Map::new();
    for (key, value) in input {
        match key.as_str() {
            "type" => match value {
                Value::String(kind) => {
                    output.insert("type".to_string(), Value::String(kind.to_ascii_lowercase()));
                }
                Value::Array(kinds) => {
                    let mut nullable = false;
                    let mut selected = None;
                    for kind in kinds {
                        if let Some(kind) = kind.as_str() {
                            if kind.eq_ignore_ascii_case("null") {
                                nullable = true;
                            } else if selected.is_none() {
                                selected = Some(kind.to_ascii_lowercase());
                            }
                        }
                    }
                    if let Some(kind) = selected {
                        output.insert("type".to_string(), Value::String(kind));
                    }
                    if nullable {
                        output.insert("nullable".to_string(), Value::Bool(true));
                    }
                }
                _ => {}
            },
            "properties" => {
                if let Value::Object(properties) = value {
                    let normalized = properties
                        .into_iter()
                        .map(|(property, schema)| (property, normalize_gemini_schema(schema)))
                        .collect();
                    output.insert("properties".to_string(), Value::Object(normalized));
                }
            }
            "items" => {
                output.insert("items".to_string(), normalize_gemini_schema(value));
            }
            "format" | "description" | "nullable" | "enum" | "maxItems" | "minItems"
            | "maxLength" | "minLength" | "pattern" | "required" => {
                output.insert(key, value);
            }
            _ => {}
        }
    }
    output
        .entry("type".to_string())
        .or_insert_with(|| Value::String("object".to_string()));
    Value::Object(output)
}

pub(crate) fn tool_definitions_anthropic() -> serde_json::Value {
    let tools = tool_specs()
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "input_schema": (spec.parameters)(),
            })
        })
        .collect::<Vec<_>>();
    json!(tools)
}

pub(crate) fn task_tool_definition_ollama(agent_names: &[String]) -> serde_json::Value {
    task_tool_definition_openai(agent_names)
}

pub(crate) fn task_tool_definition_gemini(agent_names: &[String]) -> serde_json::Value {
    let catalog = if agent_names.is_empty() {
        "No sub-agents currently available.".to_string()
    } else {
        format!("Available sub-agents: {}", agent_names.join(", "))
    };
    json!({
        "name": "task",
        "description": format!(
            "Delegate a sub-task to a specialized sub-agent that runs in an isolated context \
             with its own tool set and message history. Use this for research, code review, \
             exploration, or any task that benefits from focused attention. {catalog}"
        ),
        "parameters": gemini_tool_parameters(task_tool_parameters()),
    })
}

/// Returns true if the named tool performs no side effects (no writes, no exec).
/// Used to gate parallel execution — only read-only tool batches are safe to parallelize.
pub(crate) fn is_read_only_tool(name: &str) -> bool {
    matches!(
        name,
        TOOL_NAME_THINK
            | TOOL_NAME_READ_FILE
            | TOOL_NAME_LIST_DIR
            | TOOL_NAME_SEARCH_FILES
            | TOOL_NAME_HTTP_FETCH
    )
}

/// Returns true if the named tool is the sub-agent `task` tool.
/// This tool is handled specially by the runtime loop, not the standard execute path.
pub(crate) fn is_task_tool(name: &str) -> bool {
    name == TOOL_NAME_TASK
}

pub(crate) fn is_todos_tool(name: &str) -> bool {
    name == TOOL_NAME_TODOS
}

/// Returns true if the named tool can safely run in parallel with other parallelizable tools.
/// Parent runs share a single workspace, so this is intentionally limited to
/// built-in read-only tools until delegated tasks gain real filesystem isolation.
pub(crate) fn is_parallelizable_tool(name: &str) -> bool {
    is_read_only_tool(name)
}

/// Returns true if the named tool call can safely run in a parallel batch.
/// This includes built-in read-only tools plus cached MCP tools whose
/// descriptors are conservatively classified as read-only from their
/// name/description. Cache misses fall back to sequential execution.
pub(crate) fn is_parallelizable_tool_call(
    name: &str,
    config: &Config,
    workspace: &std::path::Path,
) -> bool {
    if is_parallelizable_tool(name) {
        return true;
    }

    mcp::is_read_only_tool_name(name, config, workspace)
}

/// Generate the `task` tool definition for OpenAI format.
/// The description is dynamically enriched with discovered sub-agent names.
pub(crate) fn task_tool_definition_openai(agent_names: &[String]) -> serde_json::Value {
    let catalog = if agent_names.is_empty() {
        "No sub-agents currently available.".to_string()
    } else {
        format!("Available sub-agents: {}", agent_names.join(", "))
    };
    json!({
        "type": "function",
        "function": {
            "name": TOOL_NAME_TASK,
            "description": format!(
                "Delegate a sub-task to a specialized sub-agent that runs in an isolated context \
                 with its own tool set and message history. Use this for research, code review, \
                 exploration, or any task that benefits from focused attention. {catalog}"
            ),
            "parameters": task_tool_parameters(),
        }
    })
}

/// Generate the `task` tool definition for Anthropic format.
pub(crate) fn task_tool_definition_anthropic(agent_names: &[String]) -> serde_json::Value {
    let catalog = if agent_names.is_empty() {
        "No sub-agents currently available.".to_string()
    } else {
        format!("Available sub-agents: {}", agent_names.join(", "))
    };
    json!({
        "name": TOOL_NAME_TASK,
        "description": format!(
            "Delegate a sub-task to a specialized sub-agent that runs in an isolated context \
             with its own tool set and message history. Use this for research, code review, \
             exploration, or any task that benefits from focused attention. {catalog}"
        ),
        "input_schema": task_tool_parameters(),
    })
}

pub(crate) fn task_tool_parameters() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "agent": {
                "type": "string",
                "minLength": 1,
                "maxLength": 100,
                "description": "The name of the sub-agent to delegate the task to"
            },
            "prompt": {
                "type": "string",
                "minLength": 1,
                "maxLength": 50000,
                "description": "Detailed task description for the sub-agent"
            }
        },
        "required": ["agent", "prompt"]
    })
}

/// Returns true if the named tool is the multi-agent `orchestrate` tool.
/// Like `task`, this tool is handled specially by the runtime loop.
pub(crate) fn is_orchestrate_tool(name: &str) -> bool {
    name == TOOL_NAME_ORCHESTRATE
}

/// Shared description body for the `orchestrate` tool. Used by both the
/// OpenAI and Anthropic definitions to guarantee identical wording.
fn orchestrate_tool_description(catalog: &str) -> String {
    format!(
        "Run a DAG of sub-agent tasks in one call. Tasks with no dependencies \
         execute in parallel; dependent tasks wait for their upstream results. \
         Reference upstream output inside prompts with {{{{results.<task_id>}}}}.\n\n\
         Use orchestrate only when you actually benefit from parallelism or \
         pipelined hand-offs (2+ independent tasks, or a produce→review→fix \
         chain). For a single delegation, call `task` instead — it is cheaper \
         and easier to debug.\n\n\
         Cost reminder: every task spawns its own sub-agent loop, so an \
         orchestration of N tasks roughly costs the sum of their individual \
         token budgets. Keep the DAG small (typically ≤5 tasks) and scope each \
         prompt tightly.\n\n\
         Example — parallel exploration then synthesis:\n\
         tasks: [{{\"id\":\"code\",\"agent\":\"explore\",\"prompt\":\"Analyze code...\"}},\n\
          {{\"id\":\"docs\",\"agent\":\"researcher\",\"prompt\":\"Research docs...\"}},\n\
          {{\"id\":\"plan\",\"agent\":\"general-coder\",\"prompt\":\"Synthesize: {{{{results.code}}}} and {{{{results.docs}}}}\",\"depends_on\":[\"code\",\"docs\"]}}]\n\n\
         Example — serial review pipeline:\n\
         tasks: [{{\"id\":\"impl\",\"agent\":\"general-coder\",\"prompt\":\"Implement...\"}},\n\
          {{\"id\":\"review\",\"agent\":\"reviewer\",\"prompt\":\"Review: {{{{results.impl}}}}\",\"depends_on\":[\"impl\"]}},\n\
          {{\"id\":\"fix\",\"agent\":\"general-coder\",\"prompt\":\"Fix: {{{{results.review}}}}\",\"depends_on\":[\"review\"]}}]\n\n\
         {catalog}"
    )
}

/// Generate the `orchestrate` tool definition for OpenAI format.
pub(crate) fn orchestrate_tool_definition_openai(agent_names: &[String]) -> serde_json::Value {
    let catalog = if agent_names.is_empty() {
        "No sub-agents currently available.".to_string()
    } else {
        format!("Available sub-agents: {}", agent_names.join(", "))
    };
    json!({
        "type": "function",
        "function": {
            "name": TOOL_NAME_ORCHESTRATE,
            "description": orchestrate_tool_description(&catalog),
            "parameters": orchestrate_tool_parameters(),
        }
    })
}

/// Generate the `orchestrate` tool definition for Anthropic format.
pub(crate) fn orchestrate_tool_definition_anthropic(agent_names: &[String]) -> serde_json::Value {
    let catalog = if agent_names.is_empty() {
        "No sub-agents currently available.".to_string()
    } else {
        format!("Available sub-agents: {}", agent_names.join(", "))
    };
    json!({
        "name": TOOL_NAME_ORCHESTRATE,
        "description": orchestrate_tool_description(&catalog),
        "input_schema": orchestrate_tool_parameters(),
    })
}

/// Generate the `orchestrate` tool definition for Ollama format (reuses OpenAI format).
pub(crate) fn orchestrate_tool_definition_ollama(agent_names: &[String]) -> serde_json::Value {
    orchestrate_tool_definition_openai(agent_names)
}

/// Generate the `orchestrate` tool definition for Gemini format.
pub(crate) fn orchestrate_tool_definition_gemini(agent_names: &[String]) -> serde_json::Value {
    let catalog = if agent_names.is_empty() {
        "No sub-agents currently available.".to_string()
    } else {
        format!("Available sub-agents: {}", agent_names.join(", "))
    };
    json!({
        "name": TOOL_NAME_ORCHESTRATE,
        "description": orchestrate_tool_description(&catalog),
        "parameters": gemini_tool_parameters(orchestrate_tool_parameters()),
    })
}

pub(crate) fn orchestrate_tool_parameters() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "tasks": {
                "type": "array",
                "minItems": 1,
                "maxItems": 20,
                "description": "Array of orchestration tasks forming a DAG. Each task specifies a sub-agent and prompt, with optional dependencies on other tasks.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 50,
                            "pattern": "^[A-Za-z0-9_-]+$",
                            "description": "Unique identifier for this task. Use only ASCII letters, digits, '_' or '-'; referenced by depends_on and {{results.<id>}} placeholders."
                        },
                        "agent": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 100,
                            "description": "Name of the sub-agent to run this task"
                        },
                        "prompt": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 50000,
                            "description": "Task prompt for the sub-agent. Use {{results.<task_id>}} to inject outputs from dependency tasks."
                        },
                        "depends_on": {
                            "type": "array",
                            "items": {
                                "type": "string",
                                "pattern": "^[A-Za-z0-9_-]+$"
                            },
                            "description": "Task IDs that must complete before this task starts. Omit or empty for tasks with no dependencies."
                        }
                    },
                    "required": ["id", "agent", "prompt"]
                }
            }
        },
        "required": ["tasks"]
    })
}

pub(crate) async fn execute_tool(
    name: &str,
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
    event_tx: Option<ToolEventSender>,
) -> ToolOutcome {
    execute_tool_with_bounded_live_events(name, args_str, config, http, workspace, event_tx, None)
        .await
}

pub(crate) async fn execute_tool_with_bounded_live_events(
    name: &str,
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
    event_tx: Option<ToolEventSender>,
    bounded_event_tx: Option<BoundedToolEventSender>,
) -> ToolOutcome {
    let start = Instant::now();

    let args: serde_json::Value = match serde_json::from_str(args_str) {
        Ok(v) => v,
        Err(e) => {
            return ToolOutcome {
                output: format!("{name} error: invalid arguments JSON: {e}"),
                is_error: true,
                duration_ms: start.elapsed().as_millis() as u64,
                subagent_snapshot: None,
            };
        }
    };

    let Some(spec) = tool_specs().iter().find(|s| s.name == name) else {
        return ToolOutcome {
            output: format!("Unknown tool: {name}"),
            is_error: true,
            duration_ms: start.elapsed().as_millis() as u64,
            subagent_snapshot: None,
        };
    };

    // Pre-validate required parameters against JSON schema
    if let Some(err) = validate_tool_args(name, &args, &(spec.parameters)()) {
        return ToolOutcome {
            output: err,
            is_error: true,
            duration_ms: start.elapsed().as_millis() as u64,
            subagent_snapshot: None,
        };
    }

    let handler_output =
        (spec.handler)(&args, config, http, workspace, event_tx, bounded_event_tx).await;
    let duration_ms = start.elapsed().as_millis() as u64;
    let is_error = handler_output
        .is_error
        .unwrap_or_else(|| is_tool_error_output(name, &handler_output.output));

    ToolOutcome {
        output: handler_output.output,
        is_error,
        duration_ms,
        subagent_snapshot: None,
    }
}

/// Check if tool output looks like an error by convention.
/// Tool functions report failures using either a generic `Error: ...` prefix
/// or a tool-specific `<tool_name> error: ...` prefix. We intentionally avoid
/// substring matching so raw file/log output is not misclassified as a tool
/// failure.
pub(crate) fn is_tool_error_output(tool_name: &str, output: &str) -> bool {
    output.starts_with("Error: ") || output.starts_with(&format!("{tool_name} error: "))
}

/// Validate required parameters against the tool's JSON schema.
/// Returns `Some(error_message)` when a required param is missing.
fn validate_required_params(tool_name: &str, args: &Value, schema: &Value) -> Option<String> {
    let required = schema.get("required")?.as_array()?;
    let obj = args.as_object();
    for req in required {
        let key = req.as_str()?;
        let present = obj.is_some_and(|o| o.get(key).is_some_and(|v| !v.is_null()));
        if !present {
            return Some(format!(
                "{tool_name} error: missing required parameter '{key}'"
            ));
        }
    }
    None
}

pub(crate) fn validate_tool_args(tool_name: &str, args: &Value, schema: &Value) -> Option<String> {
    let Some(obj) = args.as_object() else {
        return Some(format!(
            "{tool_name} error: arguments must be a JSON object"
        ));
    };

    if let Some(error) = validate_required_params(tool_name, args, schema) {
        return Some(error);
    }

    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    if schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .is_some_and(|allowed| !allowed)
    {
        for key in obj.keys() {
            if !properties.contains_key(key) {
                return Some(format!("{tool_name} error: unexpected parameter '{key}'"));
            }
        }
    }

    for (key, property_schema) in &properties {
        let Some(value) = obj.get(key) else {
            continue;
        };

        if let Some(error) = validate_value_against_schema(tool_name, key, value, property_schema) {
            return Some(error);
        }
    }

    if let Some(additional_schema) = schema
        .get("additionalProperties")
        .filter(|value| value.is_object())
    {
        for (key, value) in obj {
            if properties.contains_key(key) {
                continue;
            }
            if let Some(error) =
                validate_value_against_schema(tool_name, key, value, additional_schema)
            {
                return Some(error);
            }
        }
    }

    None
}

fn validate_value_against_schema(
    tool_name: &str,
    key: &str,
    value: &Value,
    schema: &Value,
) -> Option<String> {
    if value.is_null() {
        return Some(format!(
            "{tool_name} error: parameter '{key}' cannot be null"
        ));
    }

    match schema.get("type").and_then(Value::as_str) {
        Some("string") => validate_string_property(tool_name, key, value, schema),
        Some("integer") => validate_integer_property(tool_name, key, value, schema),
        Some("boolean") => {
            if value.is_boolean() {
                None
            } else {
                Some(format!(
                    "{tool_name} error: parameter '{key}' must be a boolean, got {}",
                    json_type_name(value)
                ))
            }
        }
        Some("object") => validate_object_property(tool_name, key, value, schema),
        Some("array") => validate_array_property(tool_name, key, value, schema),
        _ => None,
    }
}

fn validate_object_property(
    tool_name: &str,
    key: &str,
    value: &Value,
    schema: &Value,
) -> Option<String> {
    let Some(obj) = value.as_object() else {
        return Some(format!(
            "{tool_name} error: parameter '{key}' must be an object, got {}",
            json_type_name(value)
        ));
    };

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for required_key in required {
            let Some(required_key) = required_key.as_str() else {
                continue;
            };
            if !obj.contains_key(required_key) {
                return Some(format!(
                    "{tool_name} error: missing required parameter '{key}.{required_key}'"
                ));
            }
        }
    }

    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    if schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        .is_some_and(|allowed| !allowed)
    {
        for nested_key in obj.keys() {
            if !properties.contains_key(nested_key) {
                return Some(format!(
                    "{tool_name} error: parameter '{key}.{nested_key}' is not allowed"
                ));
            }
        }
    }

    for (nested_key, nested_schema) in &properties {
        let Some(nested_value) = obj.get(nested_key) else {
            continue;
        };
        let compound_key = format!("{key}.{nested_key}");
        if let Some(error) =
            validate_value_against_schema(tool_name, &compound_key, nested_value, nested_schema)
        {
            return Some(error);
        }
    }

    if let Some(additional_schema) = schema
        .get("additionalProperties")
        .filter(|value| value.is_object())
    {
        for (nested_key, nested_value) in obj {
            if properties.contains_key(nested_key) {
                continue;
            }
            let compound_key = format!("{key}.{nested_key}");
            if let Some(error) = validate_value_against_schema(
                tool_name,
                &compound_key,
                nested_value,
                additional_schema,
            ) {
                return Some(error);
            }
        }
    }

    None
}

fn validate_string_property(
    tool_name: &str,
    key: &str,
    value: &Value,
    property_schema: &Value,
) -> Option<String> {
    let Some(text) = value.as_str() else {
        return Some(format!(
            "{tool_name} error: parameter '{key}' must be a string, got {}",
            json_type_name(value)
        ));
    };

    let char_len = text.chars().count() as u64;
    if let Some(min) = property_schema.get("minLength").and_then(Value::as_u64)
        && char_len < min
    {
        return Some(format!(
            "{tool_name} error: parameter '{key}' must be at least {min} characters"
        ));
    }
    if let Some(max) = property_schema.get("maxLength").and_then(Value::as_u64)
        && char_len > max
    {
        return Some(format!(
            "{tool_name} error: parameter '{key}' must be at most {max} characters"
        ));
    }

    if let Some(pattern) = property_schema.get("pattern").and_then(Value::as_str)
        && let Ok(re) = regex::Regex::new(pattern)
        && !re.is_match(text)
    {
        return Some(format!(
            "{tool_name} error: parameter '{key}' does not match pattern '{pattern}'"
        ));
    }

    None
}

fn validate_array_property(
    tool_name: &str,
    key: &str,
    value: &Value,
    property_schema: &Value,
) -> Option<String> {
    let Some(arr) = value.as_array() else {
        return Some(format!(
            "{tool_name} error: parameter '{key}' must be an array, got {}",
            json_type_name(value)
        ));
    };

    let len = arr.len() as u64;
    if let Some(min) = property_schema.get("minItems").and_then(Value::as_u64)
        && len < min
    {
        return Some(format!(
            "{tool_name} error: parameter '{key}' must have at least {min} items"
        ));
    }
    if let Some(max) = property_schema.get("maxItems").and_then(Value::as_u64)
        && len > max
    {
        return Some(format!(
            "{tool_name} error: parameter '{key}' must have at most {max} items"
        ));
    }

    if let Some(item_schema) = property_schema.get("items") {
        for (index, item) in arr.iter().enumerate() {
            let item_key = format!("{key}[{index}]");
            if let Some(error) =
                validate_value_against_schema(tool_name, &item_key, item, item_schema)
            {
                return Some(error);
            }
        }
    }

    None
}

fn validate_integer_property(
    tool_name: &str,
    key: &str,
    value: &Value,
    property_schema: &Value,
) -> Option<String> {
    let int_value = if let Some(number) = value.as_i64() {
        number
    } else if let Some(number) = value.as_u64() {
        match i64::try_from(number) {
            Ok(number) => number,
            Err(_) => return Some(format!("{tool_name} error: parameter '{key}' is too large")),
        }
    } else {
        return Some(format!(
            "{tool_name} error: parameter '{key}' must be an integer, got {}",
            json_type_name(value)
        ));
    };

    if let Some(min) = schema_i64(property_schema, "minimum")
        && int_value < min
    {
        return Some(format!(
            "{tool_name} error: parameter '{key}' must be >= {min}"
        ));
    }
    if let Some(max) = schema_i64(property_schema, "maximum")
        && int_value > max
    {
        return Some(format!(
            "{tool_name} error: parameter '{key}' must be <= {max}"
        ));
    }

    None
}

fn schema_i64(schema: &Value, field: &str) -> Option<i64> {
    schema.get(field).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|number| i64::try_from(number).ok()))
    })
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) => {
            if number.is_i64() || number.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
#[path = "../tests/tools_tests.rs"]
mod tests;
