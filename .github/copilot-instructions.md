# LingClaw Workspace Instructions

This is **LingClaw** — a ~10000-line Rust personal AI assistant built on the **Skill + CLI** paradigm. Supports **OpenAI** and **Anthropic** APIs with auto-detection, including thinking/reasoning modes. Config via `~/.lingclaw/.lingclaw.json` (JSON config file) with first-run setup wizard.

**Skill** = LLM reasoning, system prompt, context management. **CLI** = Tool execution, safety, reliability. The entire project is one loop connecting them.

## Rules

- `src/main.rs` must stay ≤ 6000 lines. Check with `wc -l src/main.rs` before committing.
- Backend entrypoint and main loop live in `src/main.rs`. ReAct state machine (`AgentPhase`, `AgentLoopCtx`, `FinishReason`, `evaluate_finish()`, `auto_think_level()`, observation summary) lives in `src/agent.rs`. The agent loop in main.rs uses `match react_ctx.phase()` to dispatch Analyze/Act/Observe/Finish phases; the Analyze arm computes phase-adaptive think level and uses `evaluate_finish()` for structured finish decisions. CLI subcommands and setup wizard live in `src/cli.rs`. Provider streaming logic (OpenAI + Anthropic SSE, thinking blocks, conversation compression, usage tracking, prompt caching helpers) lives in `src/providers.rs`. Session prompt init/load logic, bootstrap baselines, daily memory, and avatar parsing live in `src/prompts.rs`; prompt templates live on disk in `docs/reference/templates/`. Shared tool registry lives in `src/tools/mod.rs` with implementations split into `src/tools/exec.rs`, `src/tools/fs.rs`, `src/tools/net.rs`. Configuration types (`Provider`, `Config`, `JsonConfig`, etc.) and loading logic live in `src/config.rs`. Token estimation, context budget, and message pruning live in `src/context.rs`. Hook system (`HookRegistry`, `AgentHook` trait, `AutoCompressContextHook`) lives in `src/hooks.rs`. Chat command handlers (`handle_command()`, `CommandResult`, per-command handlers) live in `src/commands.rs`.
- Each session has an isolated workspace at `~/.lingclaw/{sessionId}/workspace/` with 7 prompt files (BOOTSTRAP.md, AGENTS.md, IDENTITY.md, SOUL.md, USER.md, TOOLS.md, MEMORY.md) copied from `docs/reference/templates/` on creation, plus a `memory/` subdirectory for daily logs. New sessions also record the initial IDENTITY.md and USER.md content as per-session bootstrap baselines. Bootstrap mode loads `BOOTSTRAP.md + AGENTS.md`; once either IDENTITY.md or USER.md differs from that session baseline, the backend auto-removes `BOOTSTRAP.md` and normal mode loads `AGENTS.md + IDENTITY.md + USER.md + SOUL.md`, then that session's `MEMORY.md` and today/yesterday daily memory. Existing sessions should not recreate BOOTSTRAP.md on reconnect.
- `/new` only compresses conversation, appends to `memory/YYYY-MM-DD.md`, and clears context. It must not initialize a new session or recreate `BOOTSTRAP.md`.
- Dependencies: axum, tokio, serde, serde_json, reqwest, futures, regex, tower-http, tokio-util, chrono, base64, getrandom. Do not add more without justification.
- Rust edition 2021. Target stable Rust.
- Use `cargo clippy` and `cargo fmt` before finalizing code.
- **Mandatory code review**: After completing any code change, a code review must be performed before committing. Review scope: correctness, security (OWASP Top 10), style compliance, error handling, and test coverage. Run `cargo test` and `cargo clippy` as part of the review. No commit without review.
- Frontend (`static/index.html`) does not count toward the line budget.
- Config via `~/.lingclaw/.lingclaw.json` (JSON config file) with first-run setup wizard. Environment variables are supported as fallback overrides (e.g. `LINGCLAW_OPENAI_STREAM_INCLUDE_USAGE`, `LINGCLAW_ANTHROPIC_PROMPT_CACHING`).
- Provider-specific features use compatibility gating: official API endpoints (e.g. `api.openai.com`, `api.anthropic.com`) auto-enable features; third-party compatible gateways default to disabled but can be force-enabled via config or env var.
- Default HTTP port is `18989` (`DEFAULT_PORT` in `src/main.rs`). Linux install/start helpers live in `src/cli.rs`, and `scripts/install-linux.sh` is the documented Linux install flow.
- Model resolution must support both `provider/model` and plain model IDs. For plain IDs, prefer an exact match to the current runtime config, then same-provider candidates, with deterministic ordering.
- No `.unwrap()` in production paths — use `?` or provide fallback defaults.
- All tool exec must go through `check_dangerous_command()`. User-supplied tool paths must go through `resolve_path_checked()`; internal sandboxed path normalization uses `resolve_path()`.
- Network tools must go through `check_ssrf()` (scheme validation + private IP / DNS resolution blocking) with redirects disabled.
- Unit tests live under `src/tests/`. Each module's tests go in `src/tests/<module>_tests.rs`, included via `#[cfg(test)] #[path = "tests/<module>_tests.rs"] mod <module>_tests;` at the bottom of the corresponding source file. Do not put inline `#[cfg(test)] mod tests { ... }` blocks in production source files.

## Extending

- **New tool** → Add a `ToolSpec` entry in `src/tools/mod.rs` + parameter builder + handler wrapper. Put the implementation in `src/tools/fs.rs`, `src/tools/net.rs`, or `src/tools/exec.rs` by category. This is CLI-side work.
- **New provider** → Add SSE parsing + streaming in `src/providers.rs`, add variant to `Provider` enum in `src/config.rs`. This is Skill-side work.
- **Better reasoning** → Improve `build_system_prompt()` in `src/main.rs`, `prune_messages()` in `src/context.rs`, or edit prompt templates in `docs/reference/templates/`. For agent decision layer changes (phase transitions, finish heuristic, observation summary), edit `src/agent.rs`. This is Skill-side work.
- **New command** → Add handler function and match arm in `handle_command()` in `src/commands.rs`. This is Commands-side work.
- **New CLI subcommand** → Add match arm in `handle_cli_command()` in `src/cli.rs`. CLI subcommands run before async runtime (synchronous). Use `--serve` for foreground mode.
- **Config change** → Update `JsonConfig` / `JsonSettings` structs + `Config::load()` in `src/config.rs` + `run_setup_wizard()` in `src/cli.rs` if interactive. Add env var override in the `Config::load()` env-var block if the setting needs runtime override.
- **Session prompt change** → Edit template files in `docs/reference/templates/`, or modify `init_session_prompt_files()` / `load_session_prompt_files_with_snapshot()` in `src/prompts.rs`. This is Skill-side work.
