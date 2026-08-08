# LingClaw User Guide

[简体中文](user-guide.md) · [English](user-guide.en.md) · [Back to README](../README.en.md)

This guide covers day-to-day use of LingClaw. See [Deployment](deploy.en.md) for installation and service management, and [Configuration](configuration.en.md) for models, MCP, and S3.

## Core concepts

### Main and sessions

`main` is the permanent primary session and the implicit owner of every group. Each session has its own:

- Message history and view state
- Todo snapshot
- Model override
- MCP and bundled-skill permissions
- Workspace at `~/.lingclaw/<session-id>/workspace/`
- Prompt files, skills, agents, and memory

The backend generates a six-character alphanumeric ID when creating a session. You may rename its display label, but the ID, workspace path, and protocol references remain unchanged. Session deletion is irreversible: it removes the session archive and the entire `~/.lingclaw/<session-id>/` directory, including workspace files, prompts, skills, agents, and memory, then removes the member from every group. `main`, a currently connected session, or a running session cannot be deleted.

Navigation search matches names and IDs. The recent area shows at most 12 entries and always includes Main and the active session. Everything else appears under Earlier sessions.

Structured session state is no longer stored as separate JSON files. Messages, todos, usage, sub-agent snapshots, and group data live in `~/.lingclaw/lingclaw.db`. Prompt files, skills, agents, Structured Memory, and normal Markdown remain in their workspaces. The first launch after an upgrade migrates the old store automatically and permanently retains the original JSON under `~/.lingclaw/backups/sqlite-migration-*/`.

### Groups

A group organizes several sessions into one conversation. Main handles governance but is not dispatched as a regular member. Three target modes are available:

| Mode | Behavior |
|---|---|
| All | Dispatch to every valid member; every dispatched member must reply |
| Selected | Dispatch only to sessions checked in the member picker |
| @mention | Dispatch only to valid `@session-id` mentions; with `@all`, expanded members that are not directly mentioned may return `NO_REPLY`; sending is blocked without a valid mention |

The UI renders friendly member names, while the backend protocol always uses `@session-id`. A member starts only when it has a valid global agent model or a persisted session `/model` override.

### Models, providers, and agent roles

A provider describes an endpoint and protocol. A model describes its ID, context, output, reasoning, and input capabilities. Agent roles route each kind of work to a configured model:

- `primary` — Main conversation
- `fast` — Lightweight helper calls
- `sub-agent` — Default sub-agent
- `memory` — Structured Memory
- `reflection` — Daily Reflection
- `context` — Context compression
- `sub-agent-<name>` — Named sub-agent override

A session `/model` override affects that session only and takes priority over global `primary`. An invalid override never falls back silently.

## The workspace

### Session navigation

The desktop sidebar creates, searches, and switches sessions and groups. Settings, Usage, theme, and language live at the bottom. Desktop expansion is saved as `lingclaw.sessionDrawerExpanded`; mobile navigation stays in memory and closes after selection, backdrop click, or Escape.

Session and group create, edit, rename, delete, and member-removal flows use in-app dialogs. They support focus trapping, Escape and backdrop close, inline validation, asynchronous errors, and a busy submission state.

### Storage protection mode

If the runtime detects a SQLite I/O, integrity, or constraint failure, it enters sticky protection mode for the remainder of the process and cancels active agent/group runs. Sessions, history, usage, and independent configuration files remain readable, while message sends, uploads, and session/group/todo database mutations are disabled. Fix the disk-space, permission, or database problem and restart LingClaw; the UI does not expose raw SQL errors.

Use `lingclaw db status` for a read-only database inspection, or run `lingclaw db backup [PATH]` while the service is live to create a consistent snapshot. This command covers SQLite core data only; a complete backup also includes `.lingclaw.json`, `mcp-auth.json`, and session workspaces.

### Conversation and execution stack

User messages align right and assistant Markdown uses natural document typography. Dynamic work from each run is aggregated into one Execution Stack:

- Reasoning
- Tool call/result
- Task Plan
- Sub-agent
- Orchestration

The stack starts expanded while running and collapses to a summary when complete. A manual choice takes precedence over automatic collapse. Tool and Reasoning view controls filter by step type; hiding related content also closes its inspector or modal.

Selecting a tool step opens the Tool Inspector. It docks on wide desktops, floats as a drawer at medium widths, and becomes a bottom sheet on phones. Image output appears as a lazy-loaded gallery inside the inspector.

### Composer

- Press `Enter` to send and `Shift+Enter` for a new line.
- Type `/` as the first character to open commands; use arrows to move and `Tab` to complete.
- Type `@` in a group to open member completion; the stored protocol value is the session ID.
- Execute is the implicit default and has no extra label. A regular session can select Plan Mode from the `+` menu, after which a compact Plan chip appears. The choice stays in memory per session, so switching sessions cannot leak the mode. In groups the Plan entry remains visible but disabled with an explanation.
- The `+` menu always opens. Add image remains visible for text-only models, missing S3, uploads in progress, and protected storage, but is disabled with the exact reason.
- The lower toolbar shows the current session model and, for reasoning models, its Effort. Open it to search models by Provider and select one of that model's configured Effort levels. Model and Effort are saved atomically and restored after reload or restart. Groups hide this single-model control because every member has its own route.
- Normal text sent during an active run is queued and injected before the next Analyze phase. The stop button and `/stop` request immediate cancellation.
- When no model is available, the composer explains why and blocks regular messages. Status, help, and configuration commands remain usable.

### Todos

`todos` is the single structured task list for the current session. The frontend and runtime use whole-list replacement with optimistic `revision` checks:

- Users edit status and content in the Todo panel.
- The agent submits a complete list through the `todos` tool.
- A revision conflict returns the latest snapshot instead of overwriting newer data.
- `/clear` removes messages and todos, then advances the revision.

### Usage

Usage shows daily and lifetime tokens, model-role breakdowns, daily trends, and provider trends. Roles include Primary, Fast, Sub-Agent, Memory, Reflection, and Context. All-zero data produces one compact empty state; partial gaps replace only the corresponding module.

## Slash commands

| Command | Description |
|---|---|
| `/new` | Write a conversation summary to daily memory and clear context |
| `/model [name]` | List models or change the current session model |
| `/switch <id>` | Switch session |
| `/sessions` | List sessions |
| `/delete <id>` | Delete a non-Main, non-current session that is not running |
| `/think [level]` | Set `auto`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max` |
| `/react [on\|off]` | Toggle ReAct phase visibility |
| `/tool [on\|off]` | Toggle tool steps and persist the view state |
| `/reasoning [on\|off]` | Toggle reasoning and persist the view state |
| `/stop` | Cancel the active agent run |
| `/skills` | List tools and all discovered skills |
| `/skills-system [install\|uninstall <pattern>]` | Manage bundled skills for this session |
| `/skills-global` | List global skills |
| `/skills-session` | List session-local skills |
| `/agents` | List sub-agents, sources, and effective tools |
| `/status` | Show model, provider, phase, context, and reasoning state |
| `/system-prompt` | Show the current system prompt and estimated tokens |
| `/mcp [refresh]` | Show MCP state; `refresh` rebuilds the catalog |
| `/usage` | Show current and daily token summaries |
| `/clear` | Clear messages and todos while retaining the system prompt |
| `/memory [stats\|debug]` | Inspect Structured Memory and audit state |
| `/reflection [today\|yesterday\|list]` | Inspect Daily Reflection |
| `/help` | Show command help |

A group socket does not execute normal session slash commands. Switch back to a session first.

## Built-in tools

| Tool | Purpose |
|---|---|
| `think` | Internal reasoning note |
| `todos` | Atomically replace the session todo list |
| `exec` | Run shell commands with timeout and dangerous-command checks |
| `read_file` | Read a file or optional line range |
| `write_file` | Create or overwrite a file |
| `patch_file` | Replace an exact file fragment |
| `delete_file` | Delete a workspace file |
| `list_dir` | List a directory |
| `search_files` | Regex search within the workspace |
| `http_fetch` | HTTP GET with SSRF and redirect protection |
| `view_image` | Conditionally read a workspace PNG/JPEG |
| `task` | Delegate to one sub-agent |
| `orchestrate` | Execute a sub-agent DAG |
| `session_control` | Main-only session/group dispatch and governance in normal mode |

`view_image` is exposed only when the consuming model declares `input: ["image"]` and S3 is available. Plan Mode can use `think`, read-only file/directory search, `http_fetch`, constrained `git_inspect`, conditional `view_image`, and MCP tools enabled for the current session that explicitly declare `readOnlyHint=true` without `destructiveHint=true`. Missing MCP annotations fail closed; third-party annotations are trusted server declarations.

## Skills

A skill is a knowledge module containing `SKILL.md`. LingClaw discovers three layers, with later layers overriding names from earlier ones:

| Layer | Directory |
|---|---|
| System | `~/.lingclaw/system-skills/` |
| Global | `~/.lingclaw/skills/` |
| Session | `~/.lingclaw/<session-id>/workspace/skills/` |

Minimal format:

```markdown
---
name: my-skill
description: Explain the capability and when it should be used
---

# Instructions

Steps, constraints, and referenced resources.
```

LingClaw injects the name, source, and description first. When a task matches, the agent reads the full file and referenced resources through file tools. Bundled skills are not injected by default; enable them for the current session in Settings → Skills or with `/skills-system install`. See [Bundled Skills](system-skills.en.md) for the complete list.

## Sub-agents and orchestration

Sub-agents are discovered from three layers:

| Layer | Directory |
|---|---|
| System | `~/.lingclaw/system-agents/` |
| Global | `~/.lingclaw/agents/` |
| Session | `~/.lingclaw/<session-id>/workspace/agents/` |

Bundled agents include `explore`, `researcher`, `frontend-coder`, `backend-coder`, `general-coder`, and `reviewer`. `AGENT.md` frontmatter can set `max_turns` and `tools.allow` / `tools.deny`; filters apply to built-ins and `mcp__...` tools alike.

Model resolution order:

1. `agents.defaults.model.sub-agent-<name>`
2. `agents.defaults.model.sub-agent`
3. The parent session's effective model

A sub-agent has isolated messages, tools, and a ReAct loop. To prevent recursion and shared-list races, `task`, `orchestrate`, and session `todos` are never exposed to a sub-agent. `subAgentTimeout` limits total duration, and cancelling the parent run cancels children.

`orchestrate` runs dependency layers concurrently. Tasks after a failed dependency fail or skip according to the plan. The main stack summarizes completed and failed tasks, while details retain stages, tool chains, and results.

## Plan Mode, automatic execution outline, and reasoning

- **Plan Mode** — Runs a per-session `planning → needs_input → ready → executing → completed/failed/stopped/discarded` state machine. The plan card presents the goal, summary, revision, steps, assumptions, risks, acceptance criteria, and verification.
- **Questions and revisions** — Only blocking decisions that materially change the solution enter `needs_input`. Answers or revision feedback create a new revision under the same `plan_id`. Superseded revisions remain folded and read-only in history, and stale pages cannot mutate or approve a newer revision.
- **Approval and evidence** — Local files and directories read while planning are recorded as workspace-relative SHA-256 evidence. Constrained `git_inspect` calls record their selector and result fingerprint, so worktree, index, or commit changes invalidate the plan when they affect the original inspection. If evidence changes before approval, the user must refresh the plan or explicitly continue. Confirmation is bound to the evidence snapshot actually observed at that warning, so another change requires confirmation again; successful overrides record the affected paths. MCP and HTTP observations are not presented as re-verifiable local evidence.
- **Execution progress** — Approval does not add a synthetic user bubble. The exact revision is injected into every agent cycle, while internal `update_plan` calls update existing steps or append adaptive steps with a deviation reason. Unreported steps remain visible when a run ends instead of being fabricated as complete.
- **Recovery and boundaries** — Only a failed or stopped plan that was approved and already started execution can resume remaining work. A planning run that was stopped or interrupted by a process restart must be revised or discarded; it cannot bypass approval. When the model has not produced a new revision yet, submitted answers or revision feedback are restored in the plan card so the user can inspect and resubmit them. On startup, LingClaw recovers leftover `planning` and `executing` process states as `stopped`. A ready plan must be executed or discarded before an ordinary Execute message can start. Groups currently reject Plan Mode.
- **Automatic execution outline** — The compatibility key remains `enableTaskPlan`. It only supplies runtime guidance for ordinary Execute runs without an approved plan and is suppressed during Plan-only and approved-plan execution.
- **Think level** — Controls effort for supported reasoning models. `auto` derives a level from task signals; Auto Debug only displays the most recent local decision trace.
- **Reasoning visibility** — Changes presentation only, not provider output or agent behavior.

## Memory and context

The user maintains `MEMORY.md` in the workspace. `memory/YYYY-MM-DD.md` stores daily notes and `/new` summaries.

- Structured Memory extracts stable preferences, project context, and facts into `structured_memory.json`, with a separate audit log.
- Daily Reflection may append a short background reflection after a multi-step task completes.
- Near the context limit, LingClaw prunes or compresses older messages and emits a visible notification. Runtime persistence remains authoritative.
- Memory, Reflection, and Context helper calls count toward their Usage roles but do not consume tool images again.

## Images

### User attachments

When S3 is configured and the current session's effective model declares `input: ["image"]`, the composer `+` menu uploads PNG/JPEG files. A message accepts at most 10 images and each image is limited to 10MB. Session history stores only the object key, S3 configuration identity, name, and MIME; URLs are signed again for each request.

### Tool-image feedback

The runtime extracts magic-byte-validated PNG/JPEG data from MCP `image`, image `resource.blob`, and `view_image`, uploads it, and attaches it to the corresponding `tool` message for the next cycle. Paths, URLs, SVG, WebP, and audio embedded in ordinary text are never fetched automatically.

If an image is missing, upload fails, or the model is text-only, the original tool result still completes and a text explanation is added for the agent and inspector. A sub-agent may consume an image inside its own loop but does not forward it again to the parent.

## Next steps

- [Configure models, MCP, and S3](configuration.en.md)
- [Deploy and manage the service](deploy.en.md)
- [Understand the runtime architecture](architecture.en.md)
- [Read the HTTP and WebSocket protocol](backend-api.md) — Chinese
