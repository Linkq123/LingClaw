pub(crate) mod exec;
pub(crate) mod fs;
pub(crate) mod net;

use reqwest::Client;
use serde_json::json;
use std::path::Path;
use std::{future::Future, pin::Pin};

use crate::Config;

type ToolFuture<'a> = Pin<Box<dyn Future<Output = String> + Send + 'a>>;
type ToolHandler = for<'a> fn(&'a serde_json::Value, &'a Config, &'a Client, &'a Path) -> ToolFuture<'a>;

pub(crate) struct ToolSpec {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    prompt_line: fn(&Config) -> String,
    parameters: fn() -> serde_json::Value,
    handler: ToolHandler,
}

fn tool_parameters_think() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "thought": {
                "type": "string",
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
                "description": "Shell command to execute"
            },
            "working_dir": {
                "type": "string",
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
                "description": "File path to read"
            },
            "start_line": {
                "type": "integer",
                "description": "Starting line number (1-based, optional)"
            },
            "end_line": {
                "type": "integer",
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
                "description": "File path to write"
            },
            "content": {
                "type": "string",
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
                "description": "File path to patch"
            },
            "old_string": {
                "type": "string",
                "description": "Exact string to find (must exist in the file)"
            },
            "new_string": {
                "type": "string",
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
                "description": "Directory path (default: workspace root)"
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
                "description": "Regex pattern to search for"
            },
            "path": {
                "type": "string",
                "description": "Directory to search in (default: workspace root)"
            },
            "file_glob": {
                "type": "string",
                "description": "File name filter, e.g. '*.rs' (default: all files)"
            },
            "max_results": {
                "type": "integer",
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
                "description": "URL to fetch"
            },
            "max_bytes": {
                "type": "integer",
                "description": "Maximum response size in bytes (default: 102400)"
            }
        },
        "required": ["url"]
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
    "**read_file** — Read file contents. Supports line range (start_line, end_line)."
        .to_string()
}

fn tool_prompt_line_write_file(_: &Config) -> String {
    "**write_file** — Create or overwrite files.".to_string()
}

fn tool_prompt_line_patch_file(_: &Config) -> String {
    "**patch_file** — Find and replace exact strings in files.".to_string()
}

fn tool_prompt_line_list_dir(_: &Config) -> String {
    "**list_dir** — List directory contents with file sizes.".to_string()
}

fn tool_prompt_line_search_files(_: &Config) -> String {
    "**search_files** — Regex search across files in a directory (like grep).".to_string()
}

fn tool_prompt_line_http_fetch(_: &Config) -> String {
    "**http_fetch** — Fetch content from a URL via HTTP GET.".to_string()
}

fn tool_handler_think<'a>(args: &'a serde_json::Value, _: &'a Config, _: &'a Client, _: &'a Path) -> ToolFuture<'a> {
    Box::pin(async move { exec::tool_think(args) })
}

fn tool_handler_exec<'a>(args: &'a serde_json::Value, config: &'a Config, _: &'a Client, workspace: &'a Path) -> ToolFuture<'a> {
    Box::pin(async move { exec::tool_exec(args, config, workspace).await })
}

fn tool_handler_read_file<'a>(args: &'a serde_json::Value, config: &'a Config, _: &'a Client, workspace: &'a Path) -> ToolFuture<'a> {
    Box::pin(async move { fs::tool_read_file(args, config, workspace).await })
}

fn tool_handler_write_file<'a>(args: &'a serde_json::Value, config: &'a Config, _: &'a Client, workspace: &'a Path) -> ToolFuture<'a> {
    Box::pin(async move { fs::tool_write_file(args, config, workspace).await })
}

fn tool_handler_patch_file<'a>(args: &'a serde_json::Value, config: &'a Config, _: &'a Client, workspace: &'a Path) -> ToolFuture<'a> {
    Box::pin(async move { fs::tool_patch_file(args, config, workspace).await })
}

fn tool_handler_list_dir<'a>(args: &'a serde_json::Value, config: &'a Config, _: &'a Client, workspace: &'a Path) -> ToolFuture<'a> {
    Box::pin(async move { fs::tool_list_dir(args, config, workspace).await })
}

fn tool_handler_search_files<'a>(args: &'a serde_json::Value, config: &'a Config, _: &'a Client, workspace: &'a Path) -> ToolFuture<'a> {
    Box::pin(async move { fs::tool_search_files(args, config, workspace).await })
}

fn tool_handler_http_fetch<'a>(args: &'a serde_json::Value, config: &'a Config, http: &'a Client, _: &'a Path) -> ToolFuture<'a> {
    Box::pin(async move { net::tool_http_fetch(args, http, config).await })
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

pub(crate) async fn execute_tool(
    name: &str,
    args_str: &str,
    config: &Config,
    http: &Client,
    workspace: &Path,
) -> String {
    let args: serde_json::Value = serde_json::from_str(args_str).unwrap_or_default();
    if let Some(spec) = tool_specs().iter().find(|spec| spec.name == name) {
        (spec.handler)(&args, config, http, workspace).await
    } else {
        format!("Unknown tool: {name}")
    }
}