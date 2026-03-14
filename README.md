# 🦀 LingClaw

A personal AI assistant in ~2400 lines of Rust. Three pillars: **Skill quality + Rich CLI tools + Intelligent agent loop**.

## Features

**8 Tools** — think, exec, read_file, write_file, patch_file, list_dir, search_files, http_fetch

**Smart Agent Loop** — Multi-round tool calling (up to 20 rounds), context window management with auto-pruning, per-session model override

**Multi-Session** — Create, switch, rename, save/load sessions to disk. Each WebSocket connection gets its own session.

**Dual Provider** — Native support for OpenAI and Anthropic APIs with auto-detection

**Streaming** — Real-time SSE streaming with live WebSocket push to browser

**Config File** — JSON config at `~/.lingclaw/.lingclaw.json` with multi-provider support + first-run setup wizard

**Security** — Dangerous command detection, configurable exec timeout, output truncation

**Single Binary** — One Rust binary, one `index.html`, zero database

**Compact Modules** — `main.rs` (app loop) + `providers.rs` (LLM streaming) + `tools/` (registry + fs/net/exec implementations)

## Quick Start

```bash
cargo build --release

# First run — setup wizard guides you through provider/model config
cargo run

# Or configure via environment variables:
# OpenAI
OPENAI_API_KEY=sk-xxx cargo run

# Anthropic (auto-detected from model name)
ANTHROPIC_API_KEY=sk-ant-xxx LINGCLAW_MODEL=claude-sonnet-4-20250514 cargo run

# Open http://127.0.0.1:3000
```

## Config File

LingClaw stores config at `~/.lingclaw/.lingclaw.json`. A first-run setup wizard creates it automatically.

```json
{
  "models": {
    "providers": {
      "my-provider": {
        "baseUrl": "https://api.example.com/v1",
        "apiKey": "sk-xxx",
        "api": "openai-completions",
        "models": [
          { "id": "gpt-4o-mini" }
        ]
      }
    }
  },
  "agents": {
    "defaults": {
      "model": {
        "primary": "my-provider/gpt-4o-mini"
      }
    }
  }
}
```

Model references use `provider/model` format (e.g. `my-provider/gpt-4o-mini`). Switch at runtime with `/model`.

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `OPENAI_API_KEY` | *(required for OpenAI)* | OpenAI API key |
| `ANTHROPIC_API_KEY` | *(required for Anthropic)* | Anthropic API key (fallback: `OPENAI_API_KEY`) |
| `LINGCLAW_PROVIDER` | *auto-detect* | Force provider: `openai` or `anthropic` |
| `OPENAI_API_BASE` | provider default | API endpoint |
| `LINGCLAW_MODEL` | `gpt-4o-mini` | Model name (claude-* triggers Anthropic) |
| `LINGCLAW_PORT` | `3000` | HTTP listen port |
| `LINGCLAW_EXEC_TIMEOUT` | `30` | Shell command timeout (seconds) |
| `LINGCLAW_MAX_TOOL_ROUNDS` | `20` | Max tool call rounds per message |
| `LINGCLAW_MAX_CONTEXT_TOKENS` | `32000` | Context window token budget |

## Commands

| Command | Description |
|---|---|
| `/new` | Clear context |
| `/status` | Show agent/model/context/think status |
| `/model [name]` | Show or switch model |
| `/think [level]` | Set thinking mode (off\|minimal\|low\|medium\|high\|xhigh) |
| `/skills` | List available skills |
| `/sessions` | List all active sessions |
| `/switch <id>` | Switch session (prefix match) |
| `/rename <name>` | Rename current session |
| `/save` | Save session to disk |
| `/load [id]` | List or load saved sessions |
| `/delete <id>` | Delete a session |
| `/clear` | Clear messages |
| `/help` | Show all commands |

## Tools

| Tool | Description |
|---|---|
| `think` | Step-by-step reasoning scratchpad |
| `exec` | Shell command execution with timeout |
| `read_file` | Read files with optional line range |
| `write_file` | Create or overwrite files |
| `patch_file` | Find and replace in files |
| `list_dir` | Directory listing with metadata |
| `search_files` | Regex search across files |
| `http_fetch` | HTTP GET with size limits |

## Architecture

```
Browser ←WebSocket→ axum ←HTTP/SSE→ OpenAI / Anthropic API
                      ↕
         ┌────────────┴────────────┐
         │    Agent Loop (≤20)     │
         │  Context Manager        │
         │  Session Store          │
         └────────────┬────────────┘
                      ↕
              Tool Executor ×8
```

## API

- `GET /api/health` — Server status
- `GET /api/sessions` — List sessions
- `GET /ws` — WebSocket endpoint

## License

MIT
