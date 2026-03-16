---
applyTo: "src/**/*.rs"
description: "Rust coding conventions for LingClaw's modular backend. Use when editing src/main.rs, src/cli.rs, src/providers.rs, src/prompts.rs, or any file under src/tools/."
---
# LingClaw Rust Conventions

## Skill + CLI Architecture

All code in this backend serves one of these roles:
- **Skill** (LLM reasoning): `build_system_prompt()`, `prune_messages()`, `turn_len()`, `estimate_tokens()` in `src/main.rs`; `call_llm_stream()`, `call_llm_stream_openai()`, `call_llm_stream_anthropic()`, `call_llm_simple()`, `convert_messages_to_openai()`, `convert_messages_to_anthropic()`, `think_level_to_reasoning_effort()`, `think_level_to_budget()` in `src/providers.rs`; `TEMPLATE_FILES` (tuples with `include_str!()` embedded fallback), `templates_dir()`, `init_session_prompt_files()`, `ensure_session_workspace()`, `load_session_prompt_files_with_snapshot()`, `current_local_snapshot()`, `parse_identity_avatar()`, `resolve_avatar_to_data_uri()` in `src/prompts.rs`; prompt templates on disk in `docs/reference/templates/`
- **Agent** (decision layer): `AgentPhase` enum (`Analyze`/`Act`/`Observe`/`Finish`), `AgentLoopCtx` state tracker, `maybe_annotate_observation()`, `is_finish()`, `is_empty_finish()` in `src/agent.rs`. The state machine annotates every iteration of the agent loop in `src/main.rs` with an explicit phase so the decision path is traceable. Phase transitions use `debug_assert!` for invariant enforcement.
- **CLI** (tool execution): `ToolSpec`, `tool_specs()`, `tool_definitions()`, `execute_tool()` in `src/tools/mod.rs`; `tool_*()` implementations in `src/tools/exec.rs`, `src/tools/fs.rs`, `src/tools/net.rs`; `check_dangerous_command()`, `resolve_path()`, `resolve_path_checked()` in `src/main.rs`; `check_ssrf()`, `is_private_ip()` in `src/tools/net.rs`
- **Config** (settings layer): `JsonConfig` / `JsonSettings` / `JsonProviderConfig` / `JsonModelEntry` structs, `config_dir_path()`, `config_file_path()`, `load_config_file()`, `Config::load()`, `Config::resolve_model()`, `Config::available_models()`, `Config::find_model_entry()`, `Config::context_limit_for_model()` in `src/main.rs`; `run_setup_wizard()`, `handle_cli_command()` in `src/cli.rs`
- **Loop** (connection layer): `handle_socket()`, `handle_command()`, session persistence, WebSocket plumbing in `src/main.rs`; `active_connections` dual-state tracking, `try_claim_session()` / `claim_requested_session()` for session ownership, `detect_session_avatar_update()` / `commit_session_avatar()` for live avatar polling, save-before-remove pattern in session transitions; incremental save after every round (tool and non-tool)

When adding code, know which role it belongs to. Keep `src/main.rs` as the application loop, `src/agent.rs` for the ReAct state machine and decision layer, `src/providers.rs` for LLM streaming, `src/cli.rs` for CLI subcommands and setup wizard, and `src/tools/` for the tool registry and implementations.

## Patterns

- Tool functions: `async fn tool_xxx(args: &serde_json::Value, config: &Config, workspace: &Path) -> String` — always return String (result or error); workspace is per-session
- WebSocket sends: use `ws_send(&tx, &json!({...}))` for async send or `ws_try_send(&tx, &json!({...}))` for non-blocking send via mpsc channel; never raw `.send()`
- Path handling: use `resolve_path_checked(user_str, workspace)` for user-supplied tool paths so escapes are rejected with an explicit error; use `resolve_path()` only for internal sandboxed normalization where clamp-to-workspace behavior is intended. Never trust raw user paths; workspace is per-session (`~/.lingclaw/{sessionId}/workspace`)
- Dangerous command check: must happen before any `tokio::process::Command`
- Error handling: return descriptive `format!("tool_name error: {e}")`, no panics
- Session ownership: use `active_connections` HashSet to track live WebSocket sessions; always save session to disk before removing from HashMap; retain orphaned sessions in memory on save failure for reconnect recovery
- Shutdown: use `CancellationToken` (tokio-util) for cooperative cancellation; wrap long async calls in `tokio::select!` with `cancel.cancelled()`; use `.is_cancelled()` for sync checkpoints
- SSRF: network tools must call `check_ssrf()` before fetching; `tool_http_fetch()` builds a one-off `Client` with `redirect::Policy::none()` — never use the shared `http` client for user-controlled URLs
- UTF-8 safety: use `truncate()` (finds `is_char_boundary()` safe cut point) for all string slicing with byte limits
- Prompt flow: new sessions create all template files via `init_session_prompt_files()`. Existing sessions reconnect via `ensure_session_workspace()`, which can restore missing core templates but must NOT recreate BOOTSTRAP.md. Prompt loading uses `load_session_prompt_files_with_snapshot()`, driven by `current_local_snapshot()`, so BOOTSTRAP mode reads `BOOTSTRAP.md + AGENT.md` and normal mode reads `AGENT.md + IDENTITY.md + USER.md + SOUL.md`, then `MEMORY.md` and local today/yesterday daily memory.
- `/new` is not session initialization. It only compresses messages, writes daily memory, and replaces history with a fresh system prompt for the current session.
- Command/status UI protocol: use `progress` for in-flight command updates that must NOT clear busy state; use `success` for successful terminal command summaries that should render with success styling; use `system` for neutral command output; use `error` for failures.
- Token reporting: `estimate_tokens()` is an approximate heuristic for pruning and status display, not a tokenizer-accurate count. User-visible status should label it as an estimate.
- Model config consistency: `ResolvedModel.max_tokens` should flow into both streaming and non-streaming provider calls. `Config::resolve_model()` must resolve plain model IDs deterministically, preferring the active runtime provider/config when duplicates exist.
