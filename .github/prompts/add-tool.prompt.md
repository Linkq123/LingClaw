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

## Checklist

- [ ] Tool function name is snake_case
- [ ] JSON schema has clear `description` for each parameter
- [ ] Implementation validates all required parameters (return error string, don't panic)
- [ ] Output is truncated via `truncate()` if it could be large
- [ ] User-supplied filesystem paths go through `resolve_path_checked()`; only internal sandboxed normalization should use `resolve_path()`
- [ ] Commands go through `check_dangerous_command()` if the tool runs shell commands
- [ ] Run `cargo clippy` after implementation
- [ ] Run `wc -l src/main.rs` to check line budget (≤ 3000)
