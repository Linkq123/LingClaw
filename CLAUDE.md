# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Common commands

### Backend
- Build: `cargo build`
- Run tests: `cargo test`
- Run a single test by substring: `cargo test auto_trace` or `cargo test replay_live_round -- --nocapture`
- Lint: `cargo clippy`
- Format: `cargo fmt`
- Run the server directly: `cargo run -- --serve`

### Frontend
- Install deps: `cd frontend && npm ci`
- Dev server: `cd frontend && npm run dev`
- Build static assets into `static/`: `cd frontend && npm run build`
- Run tests: `cd frontend && npm test`
- Run one Vitest file: `cd frontend && npx vitest run tests/autoTracePanel.test.ts`
- Type-check: `cd frontend && npm run typecheck`
- Lint: `cd frontend && npm run lint`
- Format check: `cd frontend && npm run fmt:check`

### Full verification typically expected for code changes
- Backend-first changes: `cargo fmt && cargo clippy && cargo test`
- Frontend changes: `cd frontend && npm run typecheck && npm test && npm run lint && npm run build`

## Project-wide constraints

- `src/main.rs` must stay under 6000 lines.
- Do not add new dependencies without justification.
- Production Rust code should avoid `.unwrap()`.
- Run `cargo clippy` and `cargo fmt` before finalizing backend changes.
- A code review is expected before committing; review correctness, security, style, error handling, and test coverage.
- Unit tests live in `src/tests/<module>_tests.rs` and are included from the corresponding source file via `#[cfg(test)] #[path = "tests/..._tests.rs"] mod tests;`.
- `static/` is generated output from `frontend/`; do not hand-edit built assets.

## High-level architecture

LingClaw is a local-first personal AI assistant built around three layers:

- **Skill**: prompt construction, context pruning, model routing, reasoning controls, memory injection
- **CLI / Tools**: filesystem, shell, network, MCP-backed tools, safety checks
- **Loop**: WebSocket session runtime, slash commands, persistence, live replay, async background work

The runtime uses an explicit ReAct-style state machine:

- **Analyze** → model decides whether to answer or call tools
- **Act** → runtime executes tool calls, including sub-agents
- **Observe** → runtime summarizes or injects tool observations
- **Finish** → persists state, emits final events, triggers optional memory/reflection work

## Backend map

- `src/main.rs` — server entrypoint, Axum HTTP/WebSocket wiring, shared app types, local-request security checks, top-level live replay state
- `src/runtime_loop.rs` — main Analyze/Act/Observe/Finish execution loop
- `src/runtime_loop/socket_input.rs` — socket input handling while idle/busy
- `src/agent.rs` — phase/state-machine logic, task intent, working state, ephemeral task plan rules, finish heuristics, observation summarization
- `src/commands.rs` — slash command handlers like `/new`, `/status`, `/mcp`, `/memory`, `/reflection`
- `src/todos.rs` — session-scoped todos validation, optimistic revision handling, and broadcast payloads
- `src/providers.rs` — provider abstraction for OpenAI Chat Completions, OpenAI Responses, Anthropic, Ollama, and Gemini request/stream handling
- `src/context.rs` — token estimation, request budgets, pruning
- `src/hooks.rs` — lifecycle hooks, tool/LLM/command hooks, automatic context compression
- `src/memory.rs` — structured memory storage/injection and updater queue
- `src/prompts.rs` — prompt template loading, workspace prompt files, skill discovery
- `src/session_store.rs` — persisted session state and migrations
- `src/config.rs` — config loading, model resolution, env var fallbacks
- `src/image_uploads.rs` — S3-compatible image upload/signing pipeline
- `src/tools/` — built-in tool registry and implementations:
  - `mod.rs` registry/dispatch
  - `fs.rs` file tools
  - `exec.rs` shell + think tools
  - `net.rs` HTTP fetch + SSRF protection
  - `mcp.rs` MCP client for stdio/Streamable HTTP, OAuth, tools/resources/prompts, session policy, caching, and lifecycle
- `src/subagents/` — agent discovery, isolated execution, and DAG orchestration

## Frontend map

Frontend source lives in `frontend/` and builds into `static/`, which the Rust server serves directly.

- `frontend/src/main.ts` — browser entrypoint and WebSocket event switchboard
- `frontend/src/input.ts` — message composer, slash-command menu behavior, image paste/drop, and send/stop flows
- `frontend/src/slashCommands.ts` — slash command catalog, normalization helpers, and autocomplete matching
- `frontend/src/socket.ts` — connection lifecycle and reconnect behavior
- `frontend/src/state.ts` — central UI state and DOM refs
- `frontend/src/renderers/` — chat, todos, tools, reasoning, subagent, orchestration, task-plan, and auto-trace panels
- `frontend/src/renderers/todos.ts` — session-level todos panel and `/api/todos` persistence flow
- `frontend/src/handlers/stream.ts` — streamed assistant/reasoning text handling
- `frontend/src/pages/SettingsPage.tsx` and `frontend/src/pages/UsagePage.tsx` — React islands
- `frontend/src/markdown.ts` — markdown/KaTeX/highlighting pipeline
- `frontend/tests/` — Vitest coverage for frontend behavior

Most of the frontend is vanilla TypeScript with direct DOM manipulation. React is used mainly for the Settings and Usage pages.

## Runtime and data flow details that matter

- The browser talks to the backend primarily over `/ws`; live reconnect/replay behavior is an important part of correctness.
- Session-scoped todos are synchronized over the dedicated `todos_state` WebSocket event and persisted with the session; `/api/todos` uses full-list replacement plus revision conflict detection.
- The app keeps `main` as the default session, but now supports multiple persisted sessions and frontend session switching.
- OpenAI family currently has two protocol kinds: `openai-completions` (`/v1/chat/completions`) and `openai-responses` (`/v1/responses`). Both conversation paths use native upstream streaming; Responses requests set `stream: true` and map Responses SSE events into LingClaw's existing live events.
- The frontend session switcher lives in a collapsible left drawer; the Todos panel is a local visibility toggle and defaults to hidden on first load.
- Slash command autocomplete is frontend-local UI on top of the existing `/...` command transport: incomplete prefixes can be completed via keyboard or mouse before dispatch.
- Automatic context compression runs as a `BeforeAnalyze` hook in `src/hooks.rs`.
- Each top-level Analyze cycle refreshes an ephemeral rule-based `TaskPlan`; it is injected as soft guidance, emitted as a `task_plan` live event, replayed on reconnect, and not persisted as a session message.
- `/new` compresses the conversation into `memory/YYYY-MM-DD.md` and clears context; it does not create a new session.
- `/clear` clears the current message context and todo items, while advancing the todos revision so stale in-flight writes cannot repopulate the list.
- Skills are discovered from three layers: system (`docs/reference/skills/`), global (`~/.lingclaw/skills/`), and session-local (`skills/`).
- Sub-agents are discovered from the parallel three-layer `agents/` hierarchy and enable the dynamic `task` and `orchestrate` tools; they do not receive the session-scoped `todos` tool.
- Structured memory and daily reflection are optional background features driven from Finish-phase/runtime services, not part of the main foreground response path.

## Security and safety-sensitive areas

- Shell execution must go through the existing dangerous-command checks.
- User-controlled paths must go through the checked path-resolution helpers.
- Network fetches must preserve SSRF protections and redirect restrictions.
- Local-only request enforcement on HTTP/WebSocket routes is a core security boundary; do not weaken it casually.

## Documentation worth reading before larger changes

- `README.md` — product overview, architecture narrative, config examples, command behavior
- `docs/backend-api.md` — HTTP and WebSocket protocol details
- `docs/deploy.md` — install/deploy behavior, especially frontend-to-static packaging
- `docs/reference/templates/` — workspace prompt template files
- `docs/reference/skills/` and `docs/reference/agents/` — built-in skills and sub-agents
