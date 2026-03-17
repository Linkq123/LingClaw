# LingClaw Workspace Instructions

This is **LingClaw** — a ~7300-line Rust personal AI assistant built on the **Skill + CLI** paradigm. Supports **OpenAI** and **Anthropic** APIs with auto-detection, including thinking/reasoning modes. Config via `~/.lingclaw/.lingclaw.json` (JSON config file) with first-run setup wizard.

**Skill** = LLM reasoning, system prompt, context management. **CLI** = Tool execution, safety, reliability. The entire project is one loop connecting them.

## Rules

- `src/main.rs` must stay ≤ 6000 lines. Check with `wc -l src/main.rs` before committing.
- Backend entrypoint and main loop live in `src/main.rs`. ReAct state machine (`AgentPhase`, `AgentLoopCtx`, `FinishReason`, `evaluate_finish()`, `auto_think_level()`, observation summary) lives in `src/agent.rs`. The agent loop in main.rs uses `match react_ctx.phase()` to dispatch Analyze/Act/Observe/Finish phases; the Analyze arm computes phase-adaptive think level and uses `evaluate_finish()` for structured finish decisions. CLI subcommands and setup wizard live in `src/cli.rs`. Provider streaming logic lives in `src/providers.rs`. Session prompt init/load logic lives in `src/prompts.rs`; prompt templates live on disk in `docs/reference/templates/`. Shared tool registry lives in `src/tools/mod.rs` with implementations split into `src/tools/exec.rs`, `src/tools/fs.rs`, `src/tools/net.rs`.
- Each session has an isolated workspace at `~/.lingclaw/{sessionId}/workspace/` with 7 prompt files (BOOTSTRAP.md, AGENTS.md, IDENTITY.md, SOUL.md, USER.md, TOOLS.md, MEMORY.md) copied from `docs/reference/templates/` on creation, plus a `memory/` subdirectory for daily logs. New sessions also record the initial IDENTITY.md and USER.md content as per-session bootstrap baselines. Bootstrap mode loads `BOOTSTRAP.md + AGENTS.md`; once either IDENTITY.md or USER.md differs from that session baseline, the backend auto-removes `BOOTSTRAP.md` and normal mode loads `AGENTS.md + IDENTITY.md + USER.md + SOUL.md`, then that session's `MEMORY.md` and today/yesterday daily memory. Existing sessions should not recreate BOOTSTRAP.md on reconnect.
- `/new` only compresses conversation, appends to `memory/YYYY-MM-DD.md`, and clears context. It must not initialize a new session or recreate `BOOTSTRAP.md`.
- Dependencies: axum, tokio, serde, serde_json, reqwest, futures, regex, tower-http, tokio-util. Do not add more without justification.
- Rust edition 2021. Target stable Rust.
- Use `cargo clippy` and `cargo fmt` before finalizing code.
- Frontend (`static/index.html`) does not count toward the line budget.
- Config via `~/.lingclaw/.lingclaw.json` (JSON config file) with first-run setup wizard. Environment variables are supported as fallback overrides.
- Model resolution must support both `provider/model` and plain model IDs. For plain IDs, prefer an exact match to the current runtime config, then same-provider candidates, with deterministic ordering.
- No `.unwrap()` in production paths — use `?` or provide fallback defaults.
- All tool exec must go through `check_dangerous_command()`. User-supplied tool paths must go through `resolve_path_checked()`; internal sandboxed path normalization uses `resolve_path()`.
- Network tools must go through `check_ssrf()` (scheme validation + private IP / DNS resolution blocking) with redirects disabled.

## Extending

- **New tool** → Add a `ToolSpec` entry in `src/tools/mod.rs` + parameter builder + handler wrapper. Put the implementation in `src/tools/fs.rs`, `src/tools/net.rs`, or `src/tools/exec.rs` by category. This is CLI-side work.
- **New provider** → Add SSE parsing + streaming in `src/providers.rs`, add variant to `Provider` enum in `src/main.rs`. This is Skill-side work.
- **Better reasoning** → Improve `build_system_prompt()` or `prune_messages()` in `src/main.rs`, or edit prompt templates in `docs/reference/templates/`. For agent decision layer changes (phase transitions, finish heuristic, observation summary), edit `src/agent.rs`. This is Skill-side work.
- **New command** → Add match arm in `handle_command()`. This is Loop-side work.
- **New CLI subcommand** → Add match arm in `handle_cli_command()` in `src/cli.rs`. CLI subcommands run before async runtime (synchronous). Use `--serve` for foreground mode.
- **Config change** → Update `JsonConfig` / `JsonSettings` structs + `Config::load()` + `run_setup_wizard()` in `src/cli.rs` if interactive.
- **Session prompt change** → Edit template files in `docs/reference/templates/`, or modify `init_session_prompt_files()` / `load_session_prompt_files_with_snapshot()` in `src/prompts.rs`. This is Skill-side work.
