# LingClaw Workspace Instructions

This is **LingClaw** — a ~3200-line Rust personal AI assistant built on the **Skill + CLI** paradigm. Supports **OpenAI** and **Anthropic** APIs with auto-detection. Config via `~/.lingclaw/.lingclaw.json` (JSON config file) with first-run setup wizard.

**Skill** = LLM reasoning, system prompt, context management. **CLI** = Tool execution, safety, reliability. The entire project is one loop connecting them.

## Rules

- `src/main.rs` must stay ≤ 3000 lines. Check with `wc -l src/main.rs` before committing.
- Backend entrypoint and main loop live in `src/main.rs`. Provider streaming logic lives in `src/providers.rs`. Session prompt init/load logic lives in `src/prompts.rs`; prompt templates live on disk in `docs/reference/templates/`. Shared tool registry lives in `src/tools/mod.rs` with implementations split into `src/tools/exec.rs`, `src/tools/fs.rs`, `src/tools/net.rs`.
- Each session has an isolated workspace at `~/.lingclaw/{sessionId}/workspace/` with 7 prompt files (BOOTSTRAP.md, AGENT.md, IDENTITY.md, SOUL.md, USER.md, TOOLS.md, MEMORY.md) copied from `docs/reference/templates/` on creation, plus a `memory/` subdirectory for daily logs. Users customize agent behavior by editing these files.
- Dependencies: axum, tokio, serde, serde_json, reqwest, futures, regex, tower-http. Do not add more without justification.
- Rust edition 2021. Target stable Rust.
- Use `cargo clippy` and `cargo fmt` before finalizing code.
- Frontend (`static/index.html`) does not count toward the line budget.
- Config via `~/.lingclaw/.lingclaw.json` (JSON config file) with first-run setup wizard. Environment variables are supported as fallback overrides.
- No `.unwrap()` in production paths — use `?` or provide fallback defaults.
- All tool exec must go through `check_dangerous_command()` and `resolve_path()` (sandboxed — paths are canonicalized and must stay inside the session workspace).

## Extending

- **New tool** → Add a `ToolSpec` entry in `src/tools/mod.rs` + parameter builder + handler wrapper. Put the implementation in `src/tools/fs.rs`, `src/tools/net.rs`, or `src/tools/exec.rs` by category. This is CLI-side work.
- **New provider** → Add SSE parsing + streaming in `src/providers.rs`, add variant to `Provider` enum in `src/main.rs`. This is Skill-side work.
- **Better reasoning** → Improve `build_system_prompt()` or `prune_messages()` in `src/main.rs`, or edit prompt templates in `docs/reference/templates/`. This is Skill-side work.
- **New command** → Add match arm in `handle_command()`. This is Loop-side work.
- **New CLI subcommand** → Add match arm in `handle_cli_command()` in `src/main.rs`. CLI subcommands run before async runtime (synchronous). Use `--serve` for foreground mode.
- **Config change** → Update `JsonConfig` / `JsonSettings` structs + `Config::load()` + `run_setup_wizard()` if interactive.
- **Session prompt change** → Edit template files in `docs/reference/templates/`, or modify `init_session_prompt_files()` / `load_session_prompt_files()` in `src/prompts.rs`. This is Skill-side work.
