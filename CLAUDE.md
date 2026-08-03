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
- `src/session_group.rs` — session group model, validation, summaries, governance, and group replay payloads
- `src/session_control.rs` — main-only cross-session control plane tool and group socket dispatch runtime
- `src/storage/` — SQLite schema and repositories, strict legacy JSON migration, online backup/status CLI, and sticky storage protection state
- `src/providers.rs` — provider abstraction for OpenAI Chat Completions, OpenAI Responses, Anthropic, Ollama, and Gemini request/stream handling
- `src/context.rs` — token estimation, request budgets, pruning
- `src/hooks.rs` — lifecycle hooks, tool/LLM/command hooks, automatic context compression
- `src/memory.rs` — structured memory storage/injection and updater queue
- `src/prompts.rs` — prompt template loading, workspace prompt files, skill discovery
- `src/session_store.rs` — session normalization, runtime persistence adapter, and test-only legacy JSON compatibility
- `src/config.rs` — config loading, model resolution, env var fallbacks
- `src/image_uploads.rs` — S3-compatible user/tool image upload, validation, ordering, and signing pipeline
- `src/tools/` — built-in tool registry and implementations:
  - `mod.rs` registry/dispatch
  - `image_view.rs` conditional read-only `view_image` with Session-workspace and PNG/JPEG validation
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
- `frontend/src/actionDialog.ts` — typed application dialogs for Session/Group identity and membership mutations, including focus restoration, validation, busy state, and inline async failures
- `frontend/src/mobile.ts` — workspace popovers, transient mobile navigation, and responsive shell state
- `frontend/src/composerAvailability.ts` — model/Agent configuration gate for composer availability, including server-validated effective Session/Group state, sanitized model-catalog classification, revision ordering/reconnect handshakes, retry/focus state, transition handling, and localized disabled states; model-independent slash commands may bypass the gate, but `/new` must not because it can call the compression model
- `frontend/src/css/workspace.css` — shared design tokens and cross-surface workspace shell rules
- `frontend/src/css/layout.css` — session navigation, workspace header, and composer layout
- `frontend/src/css/chat.css` — message rows, Markdown typography, code toolbar, and system notices
- `frontend/src/css/pages.css` — shared Settings form controls and page-level content primitives
- `frontend/src/css/console.css` — full-screen Console shell, workspace/Console transitions, responsive navigation, and sticky save surfaces
- `frontend/src/css/models-console.css` — Provider filters, model-card grid, responsive inspector, Raw JSON, and add-provider dialog
- `frontend/src/css/usage-console.css` — session-scoped Usage metrics, SVG charts, rankings, loading, and partial-data states
- `frontend/src/css/action-dialog.css` — application dialog layout, controls, and mobile bottom-sheet presentation
- `frontend/src/renderers/` — chat, todos, tools, reasoning, subagent, orchestration, task-plan, and auto-trace panels
- `frontend/src/renderers/execution-stack.ts` — groups one top-level Agent run's reasoning, tools, task plan, sub-agents, and orchestration into a single accessible live/complete activity stack; renderers must mount top-level process panels through this layer rather than creating independent timeline cards, and tool step summaries derive image counts from the associated result
- `frontend/src/renderers/tools.ts` — tool inspector content and the shared lazy, keyboard-accessible signed-image gallery used by top-level and Sub-agent tool results
- `frontend/src/renderers/group-chat.ts` — renders Group speaker metadata and the Group-specific empty state, and maps protocol mentions to display names without changing persisted content
- `frontend/src/renderers/todos.ts` — session-level todos panel and `/api/todos` persistence flow
- `frontend/src/handlers/stream.ts` — streamed assistant/reasoning text handling
- `frontend/src/pages/lazy.ts` — lazily mounts the single full-screen Console React root and preserves the latest Settings/Usage open intent
- `frontend/src/pages/SettingsPage.tsx`, `frontend/src/pages/ModelsConsole.tsx`, and `frontend/src/pages/UsagePage.tsx` — unified Console, model-card workspace, and session-scoped SVG Usage dashboard; `openSettingsPage(sessionId?, initialSection?)` may target a Settings category and `openUsagePage(sessionId?)` targets Usage without discarding state in visited views
- `frontend/src/pages/consoleTransition.ts` — swaps the workspace and Console as sibling full-screen surfaces, including `inert`/`aria-hidden`, focus restoration, native View Transitions, CSS fallback, and reduced-motion behavior; body-level workspace overlays must mount through `frontend/src/workspacePortal.ts` so `#workspace-portal-root` is isolated with the workspace
- `frontend/src/markdown.ts` and `frontend/src/highlighter.ts` — markdown/KaTeX pipeline and the size-bounded syntax-highlighting language set
- `frontend/tests/` — Vitest coverage for frontend behavior

Most of the frontend is vanilla TypeScript with direct DOM manipulation. React is used mainly for the full-screen Console and its Settings and Usage views.

## Runtime and data flow details that matter

- The browser talks to the backend primarily over `/ws`; live reconnect/replay behavior is an important part of correctness.
- `~/.lingclaw/lingclaw.db` is the sole persistent source for Sessions, messages, Todos, Usage, Sub-agent snapshots, Groups, members, votes, messages, and runs. `.lingclaw.json`, MCP authentication, Structured Memory/Markdown, Skills, Agents, and Session Workspaces remain filesystem data. Production code must never resume JSON dual-writes after migration.
- SQLite mutations go through the async `Database` facade and its dedicated `tokio-rusqlite` thread. Keep WAL, foreign keys, `synchronous=NORMAL`, busy timeout, LingClaw `application_id`, ordered `schema_migrations`, and matching `user_version`; never claim a nonempty database that lacks LingClaw ownership metadata. Session/Group multi-table changes and cross-entity cleanup belong in one transaction. Message persistence must validate contiguous stored positions before rebuilding or comparing the fingerprinted common prefix, validate fingerprints during rebuild, and rewrite only the changed tail.
- Database filenames, sidecars, the migration journal, and `backups/` are reserved top-level LingClaw-home names and must never be accepted as Session IDs or removed as Session workspaces. Roster-changing Group operations and Session deletion share the lock order `group_roster_gate` → canonical Group gates in stable ID order → SQLite transaction; keep those gates through the resulting Group broadcasts. Commit database cleanup first, and independently verify that filesystem cleanup targets one unreserved direct child of LingClaw home.
- Missing or concurrently deleted Group members are domain conflicts, not storage failures: validate API rosters, canonicalize Windows Session aliases again inside the Group transaction, and return a normal 4xx without entering process-sticky protected mode. Existing Group aliases must share the canonical persistence gate.
- Legacy `sessions/` and `groups/` migration runs before the listener. Parse primary and `.tmp` snapshots independently per logical ID, use any valid copy, and validate only live Group membership references while preserving historical message/run Session IDs. Journal recovery must consider the canonical file, `.tmp`, and `.lingclaw-save-backup`, preferring a valid later phase so already-moved directories can never be mistaken for an empty migration. Then validate hashes, atomically move source directories to a permanent `backups/sqlite-migration-*` directory, and import plus completion metadata in one transaction. Never delete the migration backup automatically. Schema upgrades require a verified online backup before mutation and must validate the schema signature, migration ledger, quick check, and foreign keys before the migration transaction commits.
- Runtime SQLite I/O, corruption, or constraint failures enter process-sticky protected mode. Decode and rebuild persisted Session/Group values inside the `Database::read`/`blocking_read` error boundary so semantic JSON, enum, and range corruption also activates protection. Cancel active direct/group runs through their cancellation tokens without setting the user-owned `stop_requested` flag, firing `/stop` hooks, or emitting a misleading `reason: user_stop`; reject core DB mutations with stable `503 storage_protected`, broadcast `storage_status`, preserve read access and independent Config saves, and never expose raw SQL errors to the browser. Protection is cleared only by repairing the external problem and restarting.
- Global daily Usage must snapshot loaded Session IDs and counters while briefly holding the in-memory mutex, release it before SQLite I/O, then aggregate only matching `session_usage` rows for unloaded Sessions. Do not rebuild messages, images, Todos, or Sub-agent snapshots for this query.
- Session-scoped todos are synchronized over the dedicated `todos_state` WebSocket event and persisted with the session; `/api/todos` uses full-list replacement plus revision conflict detection.
- The app keeps `main` as the default session, but now supports multiple persisted sessions and frontend session switching.
- Session groups are persistent group chats. A group has its own history and run list; dispatched member sessions also receive a normal user message containing group context plus the main instruction, so both group history and target session history are intentionally written.
- Group deletion is refused while any member run is `queued` or `running`; callers should stop those runs first with group socket `{"type":"group_stop"}` or `session_control.stop` instead of relying on delete to cancel background work.
- Group `model_override_members` is diagnostic compatibility data for valid persisted `/model` overrides. Frontend target gating must consume `model_configured_members`, which already applies the final rule per member: no override may use a validated global primary, while a present but invalid override blocks fallback. The backend dispatch guard uses the same rule.
- Explicit-model enforcement is server-side as well as visual: validated explicit-primary state lives in the same immutable runtime `Config` snapshot as the resolved settings. `provider_catalog_declared` preserves whether JSON originally declared providers even when runtime environment expansion removes all of them, so Session overrides and `/model` cannot silently degrade to builtin/plain legacy routing; an exact separately configured `LINGCLAW_MODEL` remains valid. Session payloads expose `modelOverridePresent`, `effectiveModelConfigured`, and `configRevision`; one global model-configuration revision advances on both Config saves and successful `/model` changes. A revision advance must collect same-snapshot payloads for every connected Session/Group while holding the model-configuration transaction lock, then release the lock before any socket send. Frontend config, Session, and Group state must reject older revisions; a new socket's first versioned model payload may establish a lower baseline only when the backend process restarted. Ordinary messages/images, Plan execution, `/new`, busy-intervention reruns, direct dispatch, and group mention follow-ups must acquire and reuse a validated Config/Session model snapshot at the final Agent-run boundary so config hot reloads cannot reintroduce the built-in fallback. task/orchestrate must use that snapshot's Session model when no dedicated sub-agent model is configured. Structured-memory requests must likewise retain the originating run's `Arc<Config>` instead of resolving their queued model against a later hot-reloaded Config.
- Config-wide model updates use the minimal `session_model_configuration` and `group_model_configuration` events; never reuse full Session/Group payloads because names, usage, members, votes, and history have independent ordering. Initial/switch full payloads still embed a model snapshot, but `configRevision` gates only those model/capability fields: an older model revision must not discard the payload's authoritative Session/Group metadata. Group model events carry `model_member_ids` as dependency metadata and may update model readiness only for the same active roster; they also carry global `s3` / `s3_config_id` capability state so storage rotation reaches clients that remain on a Group socket, without inventing a group-wide image-model capability. Settings full-document saves use `configFileEtag` / `baseConfigFileEtag` for optimistic concurrency; this file-content hash is intentionally separate from `configRevision`, which also advances for Session `/model` changes.
- Local image uploads are bound to the originating Session/Group, WebSocket, current image/S3 capabilities, and an opaque `s3_config_id`; attachment tokens sign that configuration identity as well as the object key. While an upload or asynchronous identity change is pending, ordinary send, Plan execution, attachment mutation, and Session/Group navigation stay disabled; model-independent slash commands may remain available. Never apply an upload response after the identity, socket, capabilities, or S3 configuration changed. Persist the config identity with new uploaded images; never re-sign a stale object key under a different S3 config, and strip stale historical uploads from provider context instead of blocking every later text turn. Legacy persisted images without an identity retain their compatibility path. S3 lifecycle hot-sync state records only successfully verified configuration identities; failures and timeouts must remain eligible for retry on an otherwise identical Settings save, and a request that waited behind a newer S3 configuration must be skipped so stale policy never wins.
- Tool images are structured runtime data, never inferred from arbitrary tool text, stdout, paths, URLs, or resource links. MCP may contribute standard `image` content and image-MIME embedded `resource.blob`; `view_image` may read only checked Session-workspace PNG/JPEG files and is exposed only when the model that will consume the tool result on the next Analyze cycle supports image input and S3 is configured. For a top-level first-cycle fast-model tool call, this gate follows the primary model used by the next cycle, not the fast producer; Sub-agents use their own loop model. Keep raw bytes/Base64 out of logs, WebSocket payloads, model text, and SQLite. Sequential and read-only parallel calls reserve one shared, provider-order tool-batch image budget before Base64 decode or file-body reads, so raw image memory cannot exceed the at-most-10 attachment limit. A parallel call may wait for earlier tickets, but that wait pauses only its individual tool runtime timeout; run cancellation and the enclosing Agent/Sub-agent deadline remain active. Uploads use at most three concurrent requests while preserving tool-call and image order. Cancellation may discard pending image uploads, but any already completed tool's textual result must still be recorded before the run exits. Failures add a textual unavailable notice without changing the tool's success state. Persist only object key, S3 identity, name, and MIME, then generate fresh signed URLs for provider requests and history. A stale identity drops the image without blocking text. Provider visual observations must label images as untrusted tool data; auxiliary compression/memory/reflection calls do not consume them. Sub-agents consume their own tool images but return only their text conclusion to the parent. When a request actually contains tool images, an explicit OpenAI-compatible Chat 400/422 image-content or image-capability rejection may trigger exactly one text-only retry even if the error omits the tool role; the fallback disables further tool-image attempts for that run, including when the degraded attempt fails transiently and the Agent retry layer runs again. Unrelated schema, authentication, or rate-limit errors must not.
- `main` is the implicit owner/admin for every group and must not be included in group dispatch `members`. Promoted admins live in `admins[]`; promoted-admin member removal uses a 2/3 approval threshold over promoted admins only, while `main`/owner UI removal is direct.
- Group mentions use only `@session-id` as protocol. The UI may render valid tokens as `@Session Name`, but display names must not be parsed as routing targets. `@all` may dispatch optional replies; empty or `NO_REPLY` member outputs are not persisted as group messages.
- Group chat UI should keep process noise low: queued/running/done cards and normal member live events are not rendered as chat cards; errors, management/vote system messages, and final member replies remain visible.
- `session_control` is only available to the `main` session in execute mode. PlanOnly, non-main sessions, and sub-agents must not expose it, and the backend executor still rejects non-main calls.
- `session_control.list_sessions` is intentionally lightweight: it must not scan every session workspace for prompt summaries, Skills, MCP tools, or persona details. Use `session_control.describe_session` for one target session when detailed capability discovery is needed.
- `session_control.create_session` may create a new session with initial purpose/profile text, but must not modify an existing session's prompt identity files. Generated profile summaries must not expose secrets, MCP headers, environment variables, complete system prompts, or full persona files.
- `session_control.delete_session` must keep the `/delete` safety model: never delete `main`, an active connected session, or a session with active/queued delegated work. Commit the SQLite deletion and Group membership/vote cleanup before deleting the default Workspace; a filesystem failure may leave an orphan directory but must not leave a ghost Session.
- `session_control.dispatch` is for controlling other sessions and must reject `main` as a target, including trimmed/normalized variants, so the main run never waits on a queued run behind itself.
- OpenAI family currently has two protocol kinds: `openai-completions` (`/v1/chat/completions`) and `openai-responses` (`/v1/responses`). Both conversation paths use native upstream streaming; Responses requests set `stream: true` and map Responses SSE events into LingClaw's existing live events.
- The frontend session switcher lives in a collapsible desktop sidebar. Its recent section contains at most 12 rows total while forcing Main and the current Session to remain visible; remaining Sessions live in a collapsed earlier section. Local search must cover all Session/Group names and IDs without changing persisted data. Row rename/delete actions live in one keyboard-accessible menu. At `<=768px` the sidebar becomes a transient overlay drawer that defaults closed and must not change the persisted desktop expansion preference. Todos, Tools, Reasoning, and Auto Debug live in the view-controls popover.
- Composer model-readiness errors must keep their full localized reason in the placeholder and accessibility detail, use a neutral disabled send state, and show only a short visible status with the narrowest recovery action: open Models, open Agents, prefill `/model `, or retry configuration loading. Transient checking/identity states are announced without adding a duplicate visible sentence. `openSettingsPage` accepts the target category for these entry points.
- Settings and Usage share one lazily mounted full-screen Console root rather than modal overlays. Keep visited views mounted so category changes preserve drafts and filters; leaving the Console is the boundary for unsaved-change confirmation. The workspace remains mounted but must be `inert` and `aria-hidden` while Console is active, and return must restore the originating focus and scroll position. Desktop uses the Console sidebar, intermediate widths use its compact icon navigation, and mobile uses one category select.
- Models uses searchable, capability-filtered cards with a responsive inspector; preserve stable form keys, unknown configuration metadata, custom thinking formats, Raw JSON draft rules, validation, ETag conflict handling, and the existing save semantics. Usage requests `/api/usage?session=<id>` for its active Session and derives 7/14/30-day metrics, SVG trends/composition, and Provider/Agent-role rankings without inventing unavailable cost, latency, request-count, or cross-Session data. Refreshes retain previous data, and empty states remain scoped to the affected dimension.
- Repeated Markdown decoration must be idempotent. Each code block gets at most one local-SVG toolbar, and its copy action must read only the contained `code` text. Message timestamps use semantic `<time>` nodes and remain visible on touch devices.
- Frontend action and status icons must use the inline SVG sprite in `frontend/index.html` through typed helpers from `frontend/src/icons.ts`; keep SVGs decorative with accessible labels on their controls, and do not introduce Emoji or Unicode glyphs as UI icons.
- Top-level process UI uses one execution stack per Agent run. It may span multiple ReAct cycles, auto-collapses only when the user has not manually toggled it, filters Tool/Reasoning steps through the existing view state, and must not fabricate duration for history records that do not provide one.
- Execution stacks are direct children of the scrollable flex chat column and must remain non-shrinking; long expanded details use the stack body as the single bounded scroll container rather than nesting scroll areas inside Reasoning/Sub-agent/Orchestration bodies.
- Slash command autocomplete is frontend-local UI on top of the existing `/...` command transport: incomplete prefixes can be completed via keyboard or mouse before dispatch.
- Group mention autocomplete may display localized Session names, but selection must write the exact `@session-id` token into the composer; speaker names and mentions are decorated as text/semantic DOM outside the raw Markdown pipeline.
- Session/Group identity and member mutations in the workbench must use `actionDialog.ts`, not browser `prompt`/`confirm`. Group creation/editing excludes `main` from submitted dispatch members, while the UI presents Main separately as the permanent owner. The Group composer context owns target-mode selection and model-readiness recovery; member administration stays in the flat member drawer and its keyboard-accessible row menu.
- Automatic context compression runs as a `BeforeAnalyze` hook in `src/hooks.rs`.
- Browser `plan_mode: true` starts the per-Session Plan lifecycle owned by `src/plan.rs`. PlanOnly exposes `think`, read-only workspace/search/network tools, constrained `git_inspect`, conditional `view_image`, and only Session-enabled MCP tools explicitly annotated `readOnlyHint=true` without `destructiveHint=true`; missing MCP annotations fail closed. Tool-capable responses must terminate through internal `submit_plan`, producing either blocking questions or a structured ready revision. An endpoint that explicitly rejects or silently ignores Tool Calling may degrade once to a single legacy Markdown step with a localized warning. Group Plan Mode is unsupported and must return `group_plan_mode_unsupported` before dispatch.
- Plan actions use optimistic `plan_id + revision` control. Approval validates captured local file/directory fingerprints and exact constrained `git_inspect` selector/output fingerprints; a stale override must echo the server-issued confirmation token bound to that revision and the newly observed evidence snapshot, so a later change triggers another warning. Persist the explicit override confirmation time even when incomplete evidence produces no changed path, and preserve the first approval timestamp across Resume attempts. Approval and refresh control prompts stay ephemeral, while submitted feedback is stored on the active plan until a new revision replaces it, and none of these actions append synthetic user transcript messages. The exact approved revision is injected into every execution cycle; ordinary composer text during execution remains a deferred intervention. `planning`/`executing` require an in-memory run reservation, so startup transactionally recovers either leftover state as `stopped`; Resume is valid only for plans with a prior approval and positive execution-attempt count, while interrupted planning must be revised or discarded. Internal `update_plan` may report status or append an adaptive step with a deviation reason, but must not mutate approved goals/acceptance criteria or remove original steps. SQLite v5 owns lifecycle, immutable revisions, progress, pending feedback, the persisted initial-submission marker, and stale-override audit time; History returns at most 50 revisions as folded read-only plans while retaining the current revision.
- The compatibility setting `settings.enableTaskPlan` is presented as “Automatic Execution Outline / 自动执行提纲”. It remains an ephemeral runtime advisory signal only for ordinary Execute runs with no approved plan and must be suppressed in PlanOnly and approved-plan execution.
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

- `README.md` / `README.en.md` — concise, user-first product landing pages; keep the two languages behaviorally aligned and do not move exhaustive config, commands, structs, or protocol tables back into them
- `docs/user-guide.md` / `docs/user-guide.en.md` — Session/Group behavior, commands, tools, Skills, Sub-agents, memory, and images
- `docs/configuration.md` / `docs/configuration.en.md` — Providers, models, Agent routing, MCP, S3, environment variables, and config reload/validation semantics
- `docs/architecture.md` / `docs/architecture.en.md` — ReAct runtime, module ownership, Provider conversion, persistence, image flow, and security boundaries
- `docs/backend-api.md` — HTTP and WebSocket protocol details
- `docs/deploy.md` / `docs/deploy.en.md` — install/deploy behavior, SQLite migration/backup operations, frontend-to-static packaging, and the loopback-only network boundary
- `docs/reference/templates/` — workspace prompt template files
- `docs/reference/skills/` and `docs/reference/agents/` — built-in skills and sub-agents

When user-facing behavior changes, update the narrowest owning document and its language peer. `.lingclaw.json.example` is the canonical modern configuration example; the configuration guides should use only minimal focused fragments and document legacy compatibility fields separately. Keep `docs/backend-api.md` as the canonical wire reference instead of duplicating event tables in README. README screenshots must be produced from isolated synthetic data and must not expose real sessions, local paths, endpoints, or credentials.
