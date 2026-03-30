---
description: "Add a new tool to LingClaw. Use when extending the CLI side with a new tool capability."
---
# Add New Tool to LingClaw

This is **CLI-side work** in the Skill+CLI paradigm.

Adding a tool requires one registry entry and one implementation function:

## Steps

1. **`src/tools/mod.rs` / `tool_specs()`** — Add a `ToolSpec` entry, parameter schema builder, prompt line helper, and handler wrapper for the new tool
2. **Tool implementation** — Add the `tool_xxx()` function in the appropriate submodule:
   - `src/tools/fs.rs` — Filesystem tools (read, write, patch, list, search)
   - `src/tools/net.rs` — Network tools (http_fetch)
   - `src/tools/exec.rs` — Execution/reasoning tools (exec, think)

OpenAI tools JSON, Anthropic tools JSON, `/skills` output, and tool dispatch are generated from the shared `ToolSpec` registry.

If the new capability is coming from an external stdio MCP server rather than a built-in LingClaw tool, do not add a `ToolSpec`. Update `src/tools/mcp.rs`, `src/config.rs`, setup/docs, and the MCP runtime notes instead.

## Checklist

- [ ] Tool function name is snake_case
- [ ] JSON schema has clear `description` for each parameter
- [ ] Implementation validates schema constraints for parameters (required/type/range/length) and returns error strings instead of panicking
- [ ] Output is truncated via `truncate()` if it could be large
- [ ] User-supplied filesystem paths go through `resolve_path_checked()`; only internal sandboxed normalization should use `resolve_path()`
- [ ] If the work touches MCP-backed tools, any configured server `cwd` also goes through `resolve_path_checked()` and stays inside the session workspace
- [ ] Commands go through `check_dangerous_command()` if the tool runs shell commands
- [ ] Timeout semantics are correct: shell execution uses `exec_timeout`; generic Act/MCP defaults use `tool_timeout` unless explicitly overridden
- [ ] Run `cargo clippy` after implementation
- [ ] Run `cargo test` to verify existing tests pass
- [ ] Run `wc -l src/main.rs` to check line budget (≤ 6000)
- [ ] Perform code review before committing
