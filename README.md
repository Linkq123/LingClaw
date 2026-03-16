# 🦀 LingClaw

LingClaw is a personal AI assistant built in Rust around a simple idea: **Skill + CLI + loop**.

- **Skill**: prompt system, model routing, context pruning, thinking modes
- **CLI**: safe tool execution, sandboxed file access, network protection, install/update workflow
- **Loop**: WebSocket chat, multi-session state, streaming, slash commands, persistence

The project is currently around 5500 lines of Rust, with the main loop in `src/main.rs` kept under a hard 3000-line budget.

## Features

- **9 standard tools**: `think`, `exec`, `read_file`, `write_file`, `patch_file`, `delete_file`, `list_dir`, `search_files`, `http_fetch`
- **2 main-session admin tools**: `list_sessions`, `delete_session`
- **12 slash commands**: `/new`, `/session_new`, `/switch`, `/rename`, `/model`, `/think`, `/skills`, `/status`, `/clear`, `/help`, `/sessions`, `/delete`
- **Dual-provider model routing**: OpenAI and Anthropic, with support for both `provider/model` and plain model IDs
- **Per-session model override**: switch models at runtime with `/model`
- **Persistent multi-session workflow**: sessions are saved to disk and have isolated workspaces
- **Bootstrap + normal prompt modes**: prompt files are copied into each session workspace and loaded dynamically
- **Streaming browser UI**: Axum WebSocket backend with static frontend assets in `static/`
- **Conversation compression on `/new`**: summarizes the conversation, appends it to daily memory, then clears context
- **Security controls**: dangerous command detection, sandboxed path resolution, SSRF blocking, request redirect blocking, output/file size caps

## Quick Start

```bash
cargo build --release
cargo install --path .

# First run opens the setup wizard if no config exists
lingclaw

# Service management
lingclaw start
lingclaw stop
lingclaw restart
lingclaw status
lingclaw update
lingclaw install
lingclaw install -d /path/to/source
lingclaw health
lingclaw help
lingclaw --version
```

Open http://127.0.0.1:3000 after the service starts.

Environment variable fallback also works without a config file:

```bash
# OpenAI
OPENAI_API_KEY=sk-xxx lingclaw

# Anthropic
ANTHROPIC_API_KEY=sk-ant-xxx LINGCLAW_MODEL=claude-sonnet-4-20250514 lingclaw
```

## Configuration

LingClaw stores config at `~/.lingclaw/.lingclaw.json`. The setup wizard writes it automatically on first run.

Current example shape:

```json
{
  "settings": {
    "port": 3000,
    "execTimeout": 30,
    "maxContextTokens": 32000,
    "maxOutputBytes": 51200,
    "maxFileBytes": 204800
  },
  "models": {
    "providers": {
      "openai": {
        "baseUrl": "https://api.openai.com/v1",
        "apiKey": "sk-your-openai-key",
        "api": "openai-completions",
        "models": [
          {
            "id": "gpt-4o-mini",
            "name": "gpt-4o-mini",
            "reasoning": false,
            "input": ["text", "image"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 128000,
            "maxTokens": 16384
          }
        ]
      },
      "anthropic": {
        "baseUrl": "https://api.anthropic.com",
        "apiKey": "sk-ant-your-anthropic-key",
        "api": "anthropic",
        "models": [
          {
            "id": "claude-sonnet-4-20250514",
            "name": "claude-sonnet-4-20250514",
            "reasoning": false,
            "input": ["text", "image"],
            "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
            "contextWindow": 200000,
            "maxTokens": 8192
          }
        ]
      }
    }
  },
  "agents": {
    "defaults": {
      "model": {
        "primary": "openai/gpt-4o-mini"
      },
      "models": {
        "openai/gpt-4o-mini": {},
        "anthropic/claude-sonnet-4-20250514": {}
      }
    }
  }
}
```

Notes:

- Preferred model references are `provider/model`
- Plain model IDs are still accepted in some places, but when multiple providers expose the same ID, LingClaw requires an explicit `provider/model`
- Legacy `settings.provider`, `settings.apiKey`, and `settings.apiBase` are still read for backward compatibility, but they are no longer part of the example template

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `OPENAI_API_KEY` | provider config or empty | OpenAI API key |
| `ANTHROPIC_API_KEY` | provider config or `OPENAI_API_KEY` | Anthropic API key |
| `LINGCLAW_PROVIDER` | auto-detect | Force `openai` or `anthropic` |
| `OPENAI_API_BASE` | `https://api.openai.com/v1` | Fallback API base |
| `LINGCLAW_MODEL` | `gpt-4o-mini` | Default model |
| `LINGCLAW_PORT` | `3000` | HTTP port |
| `LINGCLAW_EXEC_TIMEOUT` | `30` | Shell command timeout in seconds |
| `LINGCLAW_MAX_CONTEXT_TOKENS` | `32000` | Default context token budget |

## Slash Commands

| Command | Description |
|---|---|
| `/new` | Compress the conversation into daily memory and clear context |
| `/session_new` | Create a new session |
| `/switch <id>` | Switch to another session |
| `/rename <name>` | Rename the current session |
| `/model [name]` | Show available models or switch the current session model |
| `/think [level]` | Set thinking mode: `auto`, `off`, `minimal`, `low`, `medium`, `high`, `xhigh` |
| `/skills` | List available skills/tools help |
| `/status` | Show resolved model, provider, API base, context estimate, and think level |
| `/clear` | Clear messages but keep the system prompt |
| `/help` | Show command help |
| `/sessions` | Main session only: list active sessions |
| `/delete <id>` | Main session only: delete a session by full ID or unique prefix |

## Tools

| Tool | Description |
|---|---|
| `think` | Internal reasoning scratchpad |
| `exec` | Run shell commands with timeout and dangerous-command filtering |
| `read_file` | Read files with optional line ranges |
| `write_file` | Create or overwrite files |
| `patch_file` | Find-and-replace patches inside files |
| `delete_file` | Delete a file from the workspace |
| `list_dir` | List directory contents |
| `search_files` | Regex search across the workspace |
| `http_fetch` | HTTP GET with SSRF protection and redirect blocking |
| `list_sessions` | Main session only: inspect session state |
| `delete_session` | Main session only: delete a session |

## Architecture

```text
Browser ←WebSocket→ axum ←HTTP/SSE→ OpenAI / Anthropic API
                      ↕
         ┌────────────┴─────────────┐
         │    Agent Loop (≤200)     │
         │  Context Manager         │
         │  Session Store           │
         │  Main Session Admin      │
         └──┬──────────┬────────────┘
            ↕          ↕
      Tool Registry   Prompt Files ×7
```

Core files:

- `src/main.rs`: config loading, sessions, slash commands, WebSocket loop, HTTP server
- `src/cli.rs`: CLI subcommands, setup wizard, install/update flow
- `src/providers.rs`: OpenAI and Anthropic request/stream handling
- `src/prompts.rs`: prompt file initialization and loading
- `src/tools/mod.rs`: tool registry and dispatch
- `src/tools/fs.rs`, `src/tools/net.rs`, `src/tools/exec.rs`: tool implementations
- `static/index.html`, `static/app.js`, `static/style.css`: browser frontend

## Session Workspace

Each session gets an isolated workspace at `~/.lingclaw/{sessionId}/workspace/` with these prompt files:

| File | Purpose |
|---|---|
| `BOOTSTRAP.md` | Initial bootstrap instructions for fresh sessions |
| `AGENT.md` | Core agent behavior |
| `IDENTITY.md` | Identity/persona and avatar source |
| `SOUL.md` | Higher-level reasoning rules |
| `USER.md` | User-specific behavior guidance |
| `TOOLS.md` | Tool usage guidance |
| `MEMORY.md` | Persistent memory guidance |

There is also a `memory/` subdirectory for daily logs such as `memory/YYYY-MM-DD.md`.

Prompt loading works in two modes:

- **Bootstrap mode**: `BOOTSTRAP.md + AGENT.md`
- **Normal mode**: `AGENT.md + IDENTITY.md + USER.md + SOUL.md`, then `MEMORY.md` and recent daily memory

## HTTP API

- `GET /api/health`: service health check
- `GET /api/sessions`: list known sessions
- `POST /api/shutdown`: authenticated local shutdown endpoint used by the CLI
- `GET /ws`: chat WebSocket endpoint

## License

MIT
