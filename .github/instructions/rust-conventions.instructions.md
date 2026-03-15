---
applyTo: "src/**/*.rs"
description: "Rust coding conventions for LingClaw's modular backend. Use when editing src/main.rs, src/cli.rs, src/providers.rs, src/prompts.rs, or any file under src/tools/."
---
# LingClaw Rust Conventions

## Skill + CLI Architecture

All code in this backend serves one of these roles:
- **Skill** (LLM reasoning): `build_system_prompt()`, `prune_messages()`, `estimate_tokens()` in `src/main.rs`; `call_llm_stream()`, `call_llm_stream_openai()`, `call_llm_stream_anthropic()`, `call_llm_simple()`, `convert_messages_to_anthropic()` in `src/providers.rs`; `TEMPLATE_FILES` (tuples with `include_str!()` embedded fallback), `templates_dir()`, `init_session_prompt_files()`, `load_session_prompt_files()` in `src/prompts.rs`; prompt templates on disk in `docs/reference/templates/`
- **CLI** (tool execution): `ToolSpec`, `tool_specs()`, `tool_definitions()`, `execute_tool()` in `src/tools/mod.rs`; `tool_*()` implementations in `src/tools/exec.rs`, `src/tools/fs.rs`, `src/tools/net.rs`; `check_dangerous_command()`, `resolve_path()` in `src/main.rs`
- **Config** (settings layer): `JsonConfig` / `JsonSettings` / `JsonProviderConfig` / `JsonModelEntry` structs, `config_dir_path()`, `config_file_path()`, `load_config_file()`, `Config::load()`, `Config::resolve_model()`, `Config::available_models()` in `src/main.rs`; `run_setup_wizard()`, `handle_cli_command()` in `src/cli.rs`
- **Loop** (connection layer): `handle_socket()`, `handle_command()`, session persistence, WebSocket plumbing in `src/main.rs`; `active_connections` dual-state tracking, `try_claim_session()` / `claim_requested_session()` for session ownership, save-before-remove pattern in session transitions

When adding code, know which role it belongs to. Keep `src/main.rs` as the application loop, `src/providers.rs` for LLM streaming, `src/cli.rs` for CLI subcommands and setup wizard, and `src/tools/` for the tool registry and implementations.

## Patterns

- Tool functions: `async fn tool_xxx(args: &serde_json::Value, config: &Config, workspace: &Path) -> String` — always return String (result or error); workspace is per-session
- WebSocket sends: use `ws_send(&mut tx, &json!({...}))` helper, never raw `.send()`
- Path handling: `resolve_path(user_str, workspace)` — canonicalizes and verifies the path stays inside the session workspace; escaping paths are clamped to workspace root with a security warning. Never trust raw user paths; workspace is per-session (`~/.lingclaw/{sessionId}/workspace`)
- Dangerous command check: must happen before any `tokio::process::Command`
- Error handling: return descriptive `format!("tool_name error: {e}")`, no panics
- Session ownership: use `active_connections` HashSet to track live WebSocket sessions; always save session to disk before removing from HashMap; retain orphaned sessions in memory on save failure for reconnect recovery
- Shutdown: use `CancellationToken` (tokio-util) for cooperative cancellation; wrap long async calls in `tokio::select!` with `cancel.cancelled()`; use `.is_cancelled()` for sync checkpoints
