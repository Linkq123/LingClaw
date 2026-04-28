pub(crate) mod exec;
pub(crate) mod fs;
pub(crate) mod mcp;
pub(crate) mod net;

use reqwest::Client;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;
use std::{future::Future, pin::Pin};

use crate::Config;

/// Structured tool execution result with metadata.
pub(crate) struct ToolOutcome {
    pub output: String,
    pub is_error: bool,
    pub duration_ms: u64,
    pub subagent_snapshot: Option<crate::SubagentHistorySnapshot>,
}

type ToolFuture<'a> = Pin<Box<dyn Future<Output = String> + Send + 'a>>;
type ToolHandler =
    for<'a> fn(&'a serde_json::Value, &'a Config, &'a Client, &'a Path) -> ToolFuture<'a>;

pub(crate) struct ToolSpec {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    prompt_line: fn(&Config) -> String,
    pub(crate) parameters: fn() -> serde_json::Value,
    handler: ToolHandler,
}

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

fn tool_parameters_exec() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "minLength": 1,
                "maxLength": 20000,
                "description": "Shell command to execute"
            },
            "working_dir": {
                "type": "string",
                "maxLength": 4096,
                "description": "Working directory (default: workspace root)"
            }
        },
        "required": ["command"]
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

fn tool_prompt_line_exec(config: &Config) -> String {
    format!(
        "**exec** — Execute shell commands (timeout: {}s). Supports custom working_dir.",
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
) -> ToolFuture<'a> {
    Box::pin(async move { exec::tool_think(args) })
}

fn tool_handler_exec<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
) -> ToolFuture<'a> {
    Box::pin(async move { exec::tool_exec(args, config, workspace).await })
}

fn tool_handler_read_file<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
) -> ToolFuture<'a> {
    Box::pin(async move { fs::tool_read_file(args, config, workspace).await })
}

fn tool_handler_write_file<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
) -> ToolFuture<'a> {
    Box::pin(async move { fs::tool_write_file(args, config, workspace).await })
}

fn tool_handler_patch_file<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
) -> ToolFuture<'a> {
    Box::pin(async move { fs::tool_patch_file(args, config, workspace).await })
}

fn tool_handler_list_dir<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
) -> ToolFuture<'a> {
    Box::pin(async move { fs::tool_list_dir(args, config, workspace).await })
}

fn tool_handler_search_files<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
) -> ToolFuture<'a> {
    Box::pin(async move { fs::tool_search_files(args, config, workspace).await })
}

fn tool_handler_http_fetch<'a>(
    args: &'a serde_json::Value,
    config: &'a Config,
    http: &'a Client,
    _: &'a Path,
) -> ToolFuture<'a> {
    Box::pin(async move { net::tool_http_fetch(args, http, config).await })
}

fn tool_handler_delete_file<'a>(
    args: &'a serde_json::Value,
    _: &'a Config,
    _: &'a Client,
    workspace: &'a Path,
) -> ToolFuture<'a> {
    Box::pin(async move { fs::tool_delete_file(args, workspace).await })
}

pub(crate) fn tool_specs() -> &'static [ToolSpec] {
    &[
        ToolSpec {
            name: "think",
            description: "Plan your approach step by step before acting on complex tasks. Use this to organize your thoughts before a series of tool calls.",
            prompt_line: tool_prompt_line_think,
            parameters: tool_parameters_think,
            handler: tool_handler_think,
        },
        ToolSpec {
            name: "exec",
            description: "Execute a shell command and return stdout + stderr. Use for running programs, builds, git, file management, etc.",
            prompt_line: tool_prompt_line_exec,
            parameters: tool_parameters_exec,
            handler: tool_handler_exec,
        },
        ToolSpec {
            name: "read_file",
            description: "Read a file's contents. Supports optional line range for large files.",
            prompt_line: tool_prompt_line_read_file,
            parameters: tool_parameters_read_file,
            handler: tool_handler_read_file,
        },
        ToolSpec {
            name: "write_file",
            description: "Create a new file or overwrite an existing file with the given content.",
            prompt_line: tool_prompt_line_write_file,
            parameters: tool_parameters_write_file,
            handler: tool_handler_write_file,
        },
        ToolSpec {
            name: "patch_file",
            description: "Find and replace a specific string in a file. The old_string must match exactly.",
            prompt_line: tool_prompt_line_patch_file,
            parameters: tool_parameters_patch_file,
            handler: tool_handler_patch_file,
        },
        ToolSpec {
            name: "list_dir",
            description: "List the contents of a directory with file type and size information.",
            prompt_line: tool_prompt_line_list_dir,
            parameters: tool_parameters_list_dir,
            handler: tool_handler_list_dir,
        },
        ToolSpec {
            name: "search_files",
            description: "Search for a regex pattern in files. Returns matching lines with file paths and line numbers, like grep.",
            prompt_line: tool_prompt_line_search_files,
            parameters: tool_parameters_search_files,
            handler: tool_handler_search_files,
        },
        ToolSpec {
            name: "http_fetch",
            description: "Fetch content from a URL using HTTP GET. Returns status code and response body.",
            prompt_line: tool_prompt_line_http_fetch,
            parameters: tool_parameters_http_fetch,
            handler: tool_handler_http_fetch,
        },
        ToolSpec {
            name: "delete_file",
            description: "Delete a file from the workspace. The path must be inside the session workspace.",
            prompt_line: tool_prompt_line_delete_file,
            parameters: tool_parameters_delete_file,
            handler: tool_handler_delete_file,
        },
    ]
}

pub(crate) fn render_tool_prompt_lines(config: &Config) -> String {
    tool_specs()
        .iter()
        .enumerate()
        .map(|(idx, spec)| format!("{}. {}", idx + 1, (spec.prompt_line)(config)))
        .collect::<Vec<_>>()
        .join("\n")
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
        "think" | "read_file" | "list_dir" | "search_files" | "http_fetch"
    )
}

/// Returns true if the named tool is the sub-agent `task` tool.
/// This tool is handled specially by the runtime loop, not the standard execute path.
pub(crate) fn is_task_tool(name: &str) -> bool {
    name == "task"
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
            "name": "task",
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
        "name": "task",
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
    name == "orchestrate"
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
            "name": "orchestrate",
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
        "name": "orchestrate",
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
        "name": "orchestrate",
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

    let output = (spec.handler)(&args, config, http, workspace).await;
    let duration_ms = start.elapsed().as_millis() as u64;
    let is_error = is_tool_error_output(name, &output);

    ToolOutcome {
        output,
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

    let properties = schema.get("properties").and_then(Value::as_object)?;

    for (key, property_schema) in properties {
        let Some(value) = obj.get(key) else {
            continue;
        };

        if value.is_null() {
            return Some(format!(
                "{tool_name} error: parameter '{key}' cannot be null"
            ));
        }

        if let Some(error) = validate_property(tool_name, key, value, property_schema) {
            return Some(error);
        }
    }

    None
}

fn validate_property(
    tool_name: &str,
    key: &str,
    value: &Value,
    property_schema: &Value,
) -> Option<String> {
    match property_schema.get("type").and_then(Value::as_str) {
        Some("string") => validate_string_property(tool_name, key, value, property_schema),
        Some("integer") => validate_integer_property(tool_name, key, value, property_schema),
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
        Some("object") => {
            if value.is_object() {
                None
            } else {
                Some(format!(
                    "{tool_name} error: parameter '{key}' must be an object, got {}",
                    json_type_name(value)
                ))
            }
        }
        Some("array") => validate_array_property(tool_name, key, value, property_schema),
        _ => None,
    }
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
