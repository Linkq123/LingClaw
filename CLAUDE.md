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

- `src/main.rs` must stay under 10000 lines.
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
- `src/session_group.rs` — persistent session group store under `~/.lingclaw/groups`, validation, summaries, and group replay payloads
- `src/session_control.rs` — main-only cross-session control plane tool and group socket dispatch runtime
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
- `frontend/src/input.ts` — message composer, slash-command and Group mention menus, image paste/drop, and send/stop flows
- `frontend/src/groupMentions.ts` — shared `@session-id` parsing, caret-aware replacement, candidate filtering, and safe post-Markdown mention decoration
- `frontend/src/slashCommands.ts` — slash command catalog, normalization helpers, and autocomplete matching
- `frontend/src/socket.ts` — connection lifecycle and reconnect behavior
- `frontend/src/state.ts` — central UI state and DOM refs
- `frontend/src/icons.ts` — typed names and helpers for the local inline SVG sprite
- `frontend/src/mobile.ts` — workspace popovers, transient mobile navigation, and responsive shell state
- `frontend/src/composerAvailability.ts` — model/Agent configuration gate for composer availability, including server-validated effective Session/Group state, sanitized model-catalog classification, revision ordering/reconnect handshakes, retry/focus state, transition handling, and localized disabled states; model-independent slash commands may bypass the gate, but `/new` must not because it can call the compression model
- `frontend/src/css/workspace.css` — final design tokens and modern workspace/responsive overrides
- `frontend/src/renderers/` — chat, todos, tools, reasoning, subagent, orchestration, task-plan, and auto-trace panels
- `frontend/src/renderers/execution-stack.ts` — groups one top-level Agent run's reasoning, tools, task plan, sub-agents, and orchestration into a single accessible live/complete activity stack; renderers must mount top-level process panels through this layer rather than creating independent timeline cards
- `frontend/src/renderers/group-chat.ts` — renders Group speaker metadata separately from Markdown message content and maps protocol mentions to display names without changing persisted content
- `frontend/src/renderers/todos.ts` — session-level todos panel and `/api/todos` persistence flow
- `frontend/src/handlers/stream.ts` — streamed assistant/reasoning text handling
- `frontend/src/pages/SettingsPage.tsx` and `frontend/src/pages/UsagePage.tsx` — React islands
- `frontend/src/markdown.ts` and `frontend/src/highlighter.ts` — markdown/KaTeX pipeline and the size-bounded syntax-highlighting language set
- `frontend/tests/` — Vitest coverage for frontend behavior

Most of the frontend is vanilla TypeScript with direct DOM manipulation. React is used mainly for the Settings and Usage pages.

## Runtime and data flow details that matter

- The browser talks to the backend primarily over `/ws`; live reconnect/replay behavior is an important part of correctness.
- Session-scoped todos are synchronized over the dedicated `todos_state` WebSocket event and persisted with the session; `/api/todos` uses full-list replacement plus revision conflict detection.
- The app keeps `main` as the default session, but now supports multiple persisted sessions and frontend session switching.
- Session groups are persistent group chats. A group has its own history and run list; dispatched member sessions also receive a normal user message containing group context plus the main instruction, so both group history and target session history are intentionally written.
- Group deletion is refused while any member run is `queued` or `running`; callers should stop those runs first with group socket `{"type":"group_stop"}` or `session_control.stop` instead of relying on delete to cancel background work.
- Group `model_override_members` is diagnostic compatibility data for valid persisted `/model` overrides. Frontend target gating must consume `model_configured_members`, which already applies the final rule per member: no override may use a validated global primary, while a present but invalid override blocks fallback. The backend dispatch guard uses the same rule.
- Explicit-model enforcement is server-side as well as visual: validated explicit-primary state lives in the same immutable runtime `Config` snapshot as the resolved settings. `provider_catalog_declared` preserves whether JSON originally declared providers even when runtime environment expansion removes all of them, so Session overrides and `/model` cannot silently degrade to builtin/plain legacy routing; an exact separately configured `LINGCLAW_MODEL` remains valid. Session payloads expose `modelOverridePresent`, `effectiveModelConfigured`, and `configRevision`; one global model-configuration revision advances on both Config saves and successful `/model` changes. A revision advance must collect same-snapshot payloads for every connected Session/Group while holding the model-configuration transaction lock, then release the lock before any socket send. Frontend config, Session, and Group state must reject older revisions; a new socket's first versioned model payload may establish a lower baseline only when the backend process restarted. Ordinary messages/images, Plan execution, `/new`, busy-intervention reruns, direct dispatch, and group mention follow-ups must acquire and reuse a validated Config/Session model snapshot at the final Agent-run boundary so config hot reloads cannot reintroduce the built-in fallback. task/orchestrate must use that snapshot's Session model when no dedicated sub-agent model is configured. Structured-memory requests must likewise retain the originating run's `Arc<Config>` instead of resolving their queued model against a later hot-reloaded Config.
- Config-wide model updates use the minimal `session_model_configuration` and `group_model_configuration` events; never reuse full Session/Group payloads because names, usage, members, votes, and history have independent ordering. Initial/switch full payloads still embed a model snapshot, but `configRevision` gates only those model/capability fields: an older model revision must not discard the payload's authoritative Session/Group metadata. Group model events carry `model_member_ids` as dependency metadata and may update model readiness only for the same active roster; they also carry global `s3` / `s3_config_id` capability state so storage rotation reaches clients that remain on a Group socket, without inventing a group-wide image-model capability. Settings full-document saves use `configFileEtag` / `baseConfigFileEtag` for optimistic concurrency; this file-content hash is intentionally separate from `configRevision`, which also advances for Session `/model` changes.
- Local image uploads are bound to the originating Session/Group, WebSocket, current image/S3 capabilities, and an opaque `s3_config_id`; attachment tokens sign that configuration identity as well as the object key. While an upload or asynchronous identity change is pending, ordinary send, Plan execution, attachment mutation, and Session/Group navigation stay disabled; model-independent slash commands may remain available. Never apply an upload response after the identity, socket, capabilities, or S3 configuration changed. Persist the config identity with new uploaded images; never re-sign a stale object key under a different S3 config, and strip stale historical uploads from provider context instead of blocking every later text turn. Legacy persisted images without an identity retain their compatibility path.
- `main` is the implicit owner/admin for every group and must not be included in group dispatch `members`. Promoted admins live in `admins[]`; promoted-admin member removal uses a 2/3 approval threshold over promoted admins only, while `main`/owner UI removal is direct.
- Group mentions use only `@session-id` as protocol. The UI may render valid tokens as `@Session Name`, but display names must not be parsed as routing targets. `@all` may dispatch optional replies; empty or `NO_REPLY` member outputs are not persisted as group messages.
- Group chat UI should keep process noise low: queued/running/done cards and normal member live events are not rendered as chat cards; errors, management/vote system messages, and final member replies remain visible.
- `session_control` is only available to the `main` session in execute mode. PlanOnly, non-main sessions, and sub-agents must not expose it, and the backend executor still rejects non-main calls.
- `session_control.list_sessions` is intentionally lightweight: it must not scan every session workspace for prompt summaries, Skills, MCP tools, or persona details. Use `session_control.describe_session` for one target session when detailed capability discovery is needed.
- `session_control.create_session` may create a new session with initial purpose/profile text, but must not modify an existing session's prompt identity files. Generated profile summaries must not expose secrets, MCP headers, environment variables, complete system prompts, or full persona files.
- `session_control.delete_session` must keep the `/delete` safety model: never delete `main`, an active connected session, or a session with active/queued delegated work; successful deletion removes both persisted JSON and the default workspace directory.
- `session_control.dispatch` is for controlling other sessions and must reject `main` as a target, including trimmed/normalized variants, so the main run never waits on a queued run behind itself.
- OpenAI family currently has two protocol kinds: `openai-completions` (`/v1/chat/completions`) and `openai-responses` (`/v1/responses`). Both conversation paths use native upstream streaming; Responses requests set `stream: true` and map Responses SSE events into LingClaw's existing live events.
- The frontend session switcher lives in a collapsible desktop sidebar. At `<=768px` it becomes a transient overlay drawer that defaults closed and must not change the persisted desktop expansion preference. Todos, Tools, Reasoning, and Auto Debug live in the view-controls popover.
- Frontend action and status icons must use the inline SVG sprite in `frontend/index.html` through typed helpers from `frontend/src/icons.ts`; keep SVGs decorative with accessible labels on their controls, and do not introduce Emoji or Unicode glyphs as UI icons.
- Top-level process UI uses one execution stack per Agent run. It may span multiple ReAct cycles, auto-collapses only when the user has not manually toggled it, filters Tool/Reasoning steps through the existing view state, and must not fabricate duration for history records that do not provide one.
- Execution stacks are direct children of the scrollable flex chat column and must remain non-shrinking; long expanded details use the stack body as the single bounded scroll container rather than nesting scroll areas inside Reasoning/Sub-agent/Orchestration bodies.
- Slash command autocomplete is frontend-local UI on top of the existing `/...` command transport: incomplete prefixes can be completed via keyboard or mouse before dispatch.
- Group mention autocomplete may display localized Session names, but selection must write the exact `@session-id` token into the composer; speaker names and mentions are decorated as text/semantic DOM outside the raw Markdown pipeline.
- Automatic context compression runs as a `BeforeAnalyze` hook in `src/hooks.rs`.
- Browser `plan_mode: true` starts `AgentRunMode::PlanOnly`: the agent may call the LLM and use only read-only tools (`think`, `read_file`, `list_dir`, `search_files`, `http_fetch`, plus read-only MCP tools enabled by the session policy). It must produce an assistant plan message, store `pending_plan`, and emit `plan_ready`; clicking “开始执行” sends `execute_plan_id`, clears the pending plan, appends a short `Proceed with the approved plan.` user message, and starts normal execute mode.
- Group dispatch can request `run_mode=plan_only`; each target session still uses its own PlanOnly tool boundary and pending-plan behavior.
- The rule-based `TaskPlan` is still a runtime advisory signal controlled by `settings.enableTaskPlan` (default false). When enabled, each top-level Analyze cycle refreshes it, injects it as soft guidance, emits it as a `task_plan` live event, and replays it on reconnect. It is not the same as PlanOnly and is not persisted as a session message.
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
- Cross-session dispatch must never elevate permissions: target sessions keep their own model overrides, MCP session policy, Skills, hooks, and TaskPlan setting.

## Documentation worth reading before larger changes

- `README.md` — product overview, architecture narrative, config examples, command behavior
- `docs/backend-api.md` — HTTP and WebSocket protocol details
- `docs/deploy.md` — install/deploy behavior, especially frontend-to-static packaging
- `docs/reference/templates/` — workspace prompt template files
- `docs/reference/skills/` and `docs/reference/agents/` — built-in skills and sub-agents
