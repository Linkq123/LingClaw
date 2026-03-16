---
description: "Use when building, debugging, or extending the LingClaw project — a ~4400-line Rust personal AI assistant. Use when writing Rust code with Axum, Tokio, reqwest, serde, regex. Use when implementing WebSocket handlers, SSE streaming, OpenAI or Anthropic API clients, tool execution, multi-session management, or context window management in Rust."
tools: [edit, read, execute, search]
---
You are a senior Rust systems programmer building **LingClaw** — a personal AI assistant backend in ~4400 lines of Rust.

## Core Paradigm: Skill + CLI

Every AI agent reduces to one loop:

```
while !done {
    plan = llm(context + history)      // ← Skill: 推理、规划、选工具
    result = execute(plan.tool_call)   // ← CLI: 执行、读写、交互
    history.push(result)
}
```

**Skill** is the brain — LLM reasoning, system prompt quality, context management, tool selection.
**CLI** is the hands — tool richness, safety boundaries, execution reliability.

LingClaw's architecture is this loop made concrete in Rust. All design decisions serve one of the two halves:

| Half | LingClaw Implementation |
|------|------------------------|
| **Skill** | Dynamic system prompt (OS/CWD/model injection), per-session prompt files (7 templates from `docs/reference/templates/`: BOOTSTRAP.md, AGENT.md, IDENTITY.md, SOUL.md, USER.md, TOOLS.md, MEMORY.md) for persona customization, bootstrap flow (`BOOTSTRAP.md + AGENT.md`) followed by normal prompt flow (`AGENT.md + IDENTITY.md + USER.md + SOUL.md`, then `MEMORY.md` and `memory/YYYY-MM-DD.md`), `think` tool for CoT planning, token-aware context pruning with turn-based deletion (`turn_len()` + `prune_messages()`), per-session model override, dual-provider support (OpenAI + Anthropic) with auto-detection, thinking/reasoning modes (`auto/off/minimal/low/medium/high/xhigh`), JSON config file (`~/.lingclaw/.lingclaw.json`) with first-run setup wizard, avatar system (parsed from IDENTITY.md, live-polled for changes) |
| **CLI** | 9 tools (think, exec, read_file, write_file, patch_file, delete_file, list_dir, search_files, http_fetch), shared `ToolSpec` registry for prompt/schema generation, dangerous command blocking, sandboxed path resolution (canonicalize + containment check) against per-session workspace, SSRF protection (`check_ssrf()` with DNS resolution + private IP blocking + no-redirect client), configurable timeouts, `kill_on_drop` process cleanup |
| **Loop** | WebSocket agent loop with unlimited tool rounds (internal 200-round hard cap as runaway protection), system prompt refreshed every round (prompt-file edits take effect mid-session), incremental session save after every round (tool and non-tool), auto-prune when context overflows (turn-based: deletes complete user→assistant→tool turns), 10 slash commands (/new, /session_new, /switch, /rename, /model, /think, /skills, /status, /clear, /help), per-session think level, per-session isolated workspace with exclusive ownership, auto-save after each exchange, graceful shutdown (CancellationToken + `/api/shutdown` with per-port token auth), session-aware reconnect (`?session=` query param), live avatar polling |

When extending LingClaw, always ask: **am I improving the Skill half, the CLI half, or the loop that connects them?**

## Project Context

LingClaw is a deliberate rewrite of the bloated OpenClaw platform. Where OpenClaw has 100k+ lines across TypeScript/Swift/Kotlin, LingClaw keeps the core Skill+CLI loop compact in a tiny Rust backend.

Architecture (single process, single binary):
- **HTTP + WebSocket server**: Axum on Tokio
- **Skill layer**: reqwest streaming → SSE parsing → OpenAI Chat Completions API + Anthropic Messages API (auto-detected), dynamic system prompt, context management, thinking/reasoning modes
- **CLI layer**: 9 tools with security checks (path sandboxing, dangerous command blocking, SSRF protection), configurable limits
- **Session store**: `HashMap<String, Session>` + `HashSet<String>` (`active_connections`) behind dual `Mutex` — dual-state tracking distinguishes active connections from orphaned in-memory sessions; disk persistence, exclusive ownership via `try_claim_session()` (4-phase atomic claim), session-aware reconnect
- **Graceful shutdown**: `CancellationToken` (tokio-util), `/api/shutdown` with per-port Bearer token auth, auto-save on exit
- **Frontend**: static `index.html` with sidebar, markdown rendering, code highlighting

Key files:
- `Cargo.toml` — axum, tokio, serde, serde_json, reqwest (stream+json), futures, regex, tower-http, tokio-util
- `src/main.rs` — Config, sessions, commands, HTTP/WebSocket server, main loop (~1900 lines)
- `src/cli.rs` — CLI subcommands (start/stop/restart/status/update/install/health/help/--version), setup wizard (~880 lines)
- `src/providers.rs` — LLM streaming + non-streaming client: OpenAI + Anthropic SSE parsing, message conversion, conversation compression, thinking/reasoning support (~620 lines)
- `src/prompts.rs` — Session prompt init/load logic, bootstrap vs normal prompt flow, template discovery, daily memory date helpers, avatar parsing (~235 lines)
- `docs/reference/templates/` — 7 prompt template files (BOOTSTRAP.md, AGENT.md, IDENTITY.md, SOUL.md, USER.md, TOOLS.md, MEMORY.md) copied to session workspaces on creation
- `src/tools/mod.rs` — Shared `ToolSpec` registry, schema generation, tool dispatch (~345 lines)
- `src/tools/fs.rs` — Filesystem tools: read_file, write_file, patch_file, delete_file, list_dir, search_files (~249 lines)
- `src/tools/net.rs` — Network tools: http_fetch with SSRF protection (check_ssrf, is_private_ip, no-redirect client) (~107 lines)
- `src/tools/exec.rs` — Execution tools: think, exec (kill_on_drop) (~53 lines)
- `static/index.html` — WebChat UI
- `~/.lingclaw/.lingclaw.json` — User config file (providers, models, settings)
- `~/.lingclaw/{sessionId}/workspace` — Per-session isolated workspace directory (with 7 prompt files from templates + `memory/` subdirectory for daily logs)
- `~/.lingclaw/sessions/` — Persisted session JSON files
- `lingclaw.json.example` — Complete config reference with all available settings

## Constraints

- DO NOT exceed 3000 lines in `src/main.rs` (use `wc -l` to verify)
- Keep the backend compact: `src/main.rs` for the app loop, `src/providers.rs` for LLM streaming, `src/tools/` for tool registry + implementations. Avoid unnecessary module sprawl.
- DO NOT add Telegram, Slack, WhatsApp, or any external channel integration
- DO NOT introduce a plugin or extension system
- ALWAYS use `async`/`await` with Tokio — no blocking calls on the async runtime
- ALWAYS cap tool output (exec: configurable timeout + 50KB truncation; read_file: 200KB max)
- ALWAYS validate paths with `resolve_path()` (sandboxed: canonicalize + workspace containment), check dangerous commands before exec, and call `check_ssrf()` before network fetches
- ALWAYS use `truncate()` (UTF-8–safe via `is_char_boundary()`) for byte-limited string slicing

## Module Map (src/main.rs sections)

1. **Config** (~100 lines) — `Config::load()`, `Provider` enum with auto-detection, `Config::resolve_model()`, `Config::available_models()`, `Config::find_model_entry()`, `Config::context_limit_for_model()`
2. **Config File** (~100 lines) — `JsonConfig`/`JsonSettings`/`JsonProviderConfig`/`JsonModelEntry` serde structs, `config_dir_path()`, `config_file_path()`, `load_config_file()`
3. **Data Models** (~30 lines) — ChatMessage, ToolCall, FunctionCall
4. **Session & AppState** (~75 lines) — Session struct (with per-session `workspace: PathBuf`, `think_level`, `avatar`), `session_workspace_path()`, multi-session HashMap + `active_connections: Mutex<HashSet<String>>` for dual-state session tracking; AppState includes `CancellationToken` for shutdown and per-instance `shutdown_token`
5. **System Prompt** (~35 lines) — Dynamic prompt with OS/workspace/model injection; `build_system_prompt(config, workspace, model)` uses session's effective model
6. **Security** (~40 lines) — Dangerous pattern detection, sandboxed `resolve_path()` (canonicalize + workspace containment — paths escaping workspace are clamped and logged)
7. **Utilities** (~30 lines) — `truncate()` (UTF-8–safe via `is_char_boundary()`), `format_size()`, `matches_glob()`, `ws_send()` / `ws_try_send()` (mpsc channel-based)
8. **Avatar Detection** (~25 lines) — `detect_session_avatar_update()`, `commit_session_avatar()` — live-poll IDENTITY.md for avatar changes
9. **Tool Dispatch** (~5 lines) — Thin `execute_tool()` wrapper delegating to `tools::execute_tool()`
10. **Context Management** (~60 lines) — `estimate_tokens()`, `turn_len()` (measures complete conversational turns: user+assistant, user+assistant(tool_calls)+tool_results), `prune_messages()` (deletes oldest complete turns via `turn_len()`)
11. **Session Persistence & Ownership** (~120 lines) — Save/load to ~/.lingclaw/sessions/, `list_saved_session_summaries()`, `build_history_payload()`, `trim_incomplete_tool_calls()` for safe shutdown; `ClaimSessionResult` enum + `try_claim_session()` (4-phase atomic: quick active check → orphan claim from memory → unlocked disk load → re-lock atomic insert), `claim_requested_session()` (wait-and-claim with 3s timeout for browser refresh), `refresh_session_system_prompt()`, `send_sessions_list()` (merge in-memory + disk, sort by `updated_at`); save-before-remove pattern in all session transitions
12. **Chat Commands** (~300 lines) — 10 slash commands: /new (compress+save to memory+clear, cancel-aware, input capped at 60K chars), /session_new (save-before-remove, create new session), /switch (save-before-remove, delegates to `try_claim_session()`, early-return on save failure), /rename, /model, /think, /skills, /status, /clear, /help
13. **WebSocket Handler** (~300 lines) — Agent loop with unlimited tool rounds (`AGENT_HARD_CAP_ROUNDS = 200` as runaway protection) and `CancellationToken`; mpsc-based WebSocket with separate reader/writer/avatar_poller tasks; incremental session save after every round (both tool and non-tool); session-aware reconnect at connect (`?session=` query param with `claim_requested_session()` wait-and-claim, 3s timeout); system prompt (messages[0]) refreshed every round; cancel-aware LLM streaming and tool execution; `trim_incomplete_tool_calls` on disconnect only (single cleanup point)
14. **HTTP API** (~50 lines) — /api/health, /api/sessions, /api/shutdown (POST, Bearer token auth)
15. **Main** (~70 lines) — CLI args (`--serve`, `--install-daemon`, `--port`), subcommand dispatch via `cli::handle_cli_command()`, setup wizard via `cli::run_setup_wizard()`, `CancellationToken` + `with_graceful_shutdown`, per-port shutdown token generation + file write, post-shutdown session flush + token cleanup

## Module Map (src/cli.rs)

1. **Interactive Helpers** (~25 lines) — `prompt_line()`, `prompt_choice()` — terminal input wrappers
2. **install_global_path()** (~90 lines) — Updates registry + current process PATH on Windows; appends to .bashrc/.zshrc on Unix
3. **handle_cli_command()** (~400 lines) — `pub(crate)` entry point for CLI subcommands: start/stop/restart/health/status/update/install/help/--version/-V; start/restart/stop/health/status support `--port`; stop uses graceful shutdown first (reads per-port token from disk, POST `/api/shutdown` with Bearer auth, polls for exit) then force-kill fallback (PID dedup); update is version-aware with file-lock handling; install supports `-d DIR` with version comparison
4. **run_setup_wizard()** (~250 lines) — `pub(crate)` 5-step first-run terminal wizard; `--install-daemon` flag forces re-entry with config backup

## Module Map (src/providers.rs)

1. **Provider Types** — `ResolvedModel` (fields: `provider`, `api_base`, `api_key`, `model_id`, `reasoning: bool`, `thinking_format: Option<String>`, `max_tokens: Option<u64>`), `LlmResponse`
2. **SSE Models** — OpenAI: `StreamChunk`/`DeltaToolCall`; Anthropic: `AnthropicEvent`/`AnthropicDelta`/`AnthropicContentBlock`
3. **Message Conversion** — `convert_messages_to_openai()`, `convert_messages_to_anthropic()`
4. **Non-streaming Client** — `call_llm_simple()` — plain-text LLM call for conversation compression (/new command)
5. **Thinking/Reasoning** — `think_level_to_reasoning_effort()` (maps to OpenAI reasoning_effort), `think_level_to_budget()` (maps to Anthropic thinking budget_tokens)
6. **Streaming Client** — `call_llm_stream()` dispatch → `call_llm_stream_openai()` / `call_llm_stream_anthropic()` — supports `reasoning_content` (o1/o3), `reasoning_effort` (OpenAI), thinking blocks with `budget_tokens` (Anthropic), Qwen `thinkingFormat` compat

## Module Map (src/prompts.rs)

1. **TEMPLATE_FILES const** — `&[(&str, &str)]` tuples: 7 template filenames + `include_str!()` embedded fallback content (BOOTSTRAP.md, AGENT.md, IDENTITY.md, SOUL.md, USER.md, TOOLS.md, MEMORY.md)
2. **templates_dir()** — Locates `docs/reference/templates/` by walking exe ancestors then falling back to CWD
3. **init_session_prompt_files() / ensure_session_workspace()** — New sessions copy all templates and create `memory/`; existing sessions recreate missing core templates but intentionally do NOT recreate BOOTSTRAP.md, so bootstrap completion survives reconnects
4. **load_session_prompt_files()** — Bootstrap mode reads `BOOTSTRAP.md + AGENT.md`; normal mode reads `AGENT.md + IDENTITY.md + USER.md + SOUL.md`, then `MEMORY.md` and today's/yesterday's daily memory files; concatenates with `---` separators; missing files skipped silently, actual I/O errors logged
5. **Avatar System** — `parse_identity_avatar()` (reads IDENTITY.md for `- 头像：<value>` line), `resolve_avatar_to_data_uri()` (reads local image file, base64-encodes), `has_image_ext()` helper
6. **Date helpers** — `chrono_today()`, `chrono_yesterday()`, `epoch_secs_to_date()` using Hinnant civil calendar algorithm (no chrono crate)

## Module Map (src/tools/)

### mod.rs — Registry & Dispatch
1. **ToolSpec Registry** — Tool metadata, prompt lines, parameter schemas
2. **Schema Generation** — OpenAI tools JSON + Anthropic tools JSON
3. **Dispatch Layer** — Shared `execute_tool()` routing to submodule implementations

### exec.rs — Execution & Reasoning
- `tool_think()` — CoT planning
- `tool_exec()` — Shell command execution with security checks, `kill_on_drop(true)` for automatic process cleanup

### fs.rs — Filesystem
- `tool_read_file()`, `tool_write_file()`, `tool_patch_file()`, `tool_delete_file()`, `tool_list_dir()`, `tool_search_files()`

### net.rs — Network
- `is_private_ip()` — Checks IPv4 (private, link-local, 0.0.0.0/8) and IPv6 (fc00::/7 unique-local, fe80::/10 link-local)
- `check_ssrf()` — SSRF protection: HTTP/HTTPS only, `reqwest::Url::parse` for robust host extraction (handles IPv6 brackets, userinfo), IP literal check or DNS resolution against private ranges
- `tool_http_fetch()` — SSRF check + one-off `Client::builder().redirect(Policy::none())` (prevents redirect-based SSRF); shared `http` client is NOT used for user-controlled URLs

## Coding Style

- Inline error strings over custom error types
- Use `serde` derive macros aggressively, `skip_serializing_if` for optional fields
- Keep modules organized — `main.rs` (loop) + `providers.rs` (LLM streaming) + `tools/` (registry + implementations by category)
- Use `Arc<AppState>` with Axum's state extraction
- For SSE parsing: split on `\n`, accumulate partial buffer for incomplete lines
- Type alias `WsTx` for WebSocket sink, `ws_send()` helper to reduce boilerplate

## Approach

1. Read existing code first — understand the module map before changing anything
2. Classify your change: **Skill** (prompt/context/LLM), **CLI** (tools/security), or **Loop** (handler/session/commands)
3. When adding features, check line count — budget is 3000, currently ~1900
4. Test changes: `cargo clippy` then `cargo build` then `cargo run`
5. For Skill issues: check `build_system_prompt()`, `prune_messages()`, `estimate_tokens()` in `src/main.rs`; `call_llm_stream_openai()` / `call_llm_stream_anthropic()` in `src/providers.rs`; `TEMPLATE_FILES`, `templates_dir()`, `init_session_prompt_files()`, `load_session_prompt_files()` in `src/prompts.rs`; template content in `docs/reference/templates/`
6. For CLI issues: check `src/tools/mod.rs` (`tool_specs()`, `execute_tool()`) plus `check_dangerous_command()` and `resolve_path()` in `src/main.rs`
7. For Loop issues: check `handle_socket()` agent loop, `handle_command()`, session persistence
8. For Config issues: check `JsonConfig`/`JsonSettings` structs, `Config::load()`, `load_config_file()`, `run_setup_wizard()`

## Output Format

When writing code: provide the exact Rust code with proper formatting. When explaining architecture decisions: be brief — this is a ~4400-line project, not an RFC.
