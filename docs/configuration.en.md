# LingClaw Configuration

[简体中文](configuration.md) · [English](configuration.en.md) · [Back to README](../README.en.md)

LingClaw reads `~/.lingclaw/.lingclaw.json`. The first-run Setup Wizard can create a basic file; after that, prefer the web Settings workspace. See [`.lingclaw.json.example`](../.lingclaw.json.example) for the canonical modern configuration example. It intentionally omits legacy single-provider compatibility fields so new installations do not propagate them.

## First configuration

Regular agent chat requires both conditions:

1. `models.providers` contains at least one provider and model that is available at runtime.
2. `agents.defaults.model.primary` or the current session `/model` refers to one of those models.

LingClaw retains an internal compatibility fallback when parsing legacy configuration, but that fallback does not count as explicit user configuration and never unlocks normal sending.

Minimal OpenAI-compatible configuration:

```json
{
  "models": {
    "providers": {
      "openai": {
        "baseUrl": "https://api.openai.com/v1",
        "apiKey": "${OPENAI_API_KEY}",
        "api": "openai-completions",
        "models": [
          {
            "id": "gpt-4o-mini",
            "name": "gpt-4o-mini",
            "reasoning": false,
            "input": ["text", "image"],
            "contextWindow": 128000,
            "maxTokens": 16384
          }
        ]
      }
    }
  },
  "agents": {
    "defaults": {
      "model": {
        "primary": "openai/gpt-4o-mini"
      }
    }
  }
}
```

Set the environment variable and start:

```powershell
$env:OPENAI_API_KEY = "sk-..."
lingclaw restart
```

```bash
export OPENAI_API_KEY="sk-..."
lingclaw restart
```

Settings validates, atomically writes, and applies a new runtime snapshot. Editing the file directly does not hot-reload the current process; restart LingClaw or save again through Settings.

## settings

Common fields:

| Field | Default | Description |
|---|---:|---|
| `port` | `18989` | Local HTTP/WebSocket port |
| `execTimeout` | `30` | `exec` timeout in seconds |
| `toolTimeout` | `30` | Timeout for non-shell tools and selected helper calls |
| `subAgentTimeout` | `300` | Total sub-agent timeout; `0` disables the time limit |
| `maxLlmRetries` | `2` | Retries for 429, 5xx, connection, and timeout errors; max 10 |
| `maxContextTokens` | `32000` | Context budget when the model does not override it |
| `maxOutputBytes` | `51200` | Tool output byte limit |
| `maxFileBytes` | `204800` | File-tool byte limit |
| `openaiStreamIncludeUsage` | `false` | Request OpenAI-compatible stream usage |
| `anthropicPromptCaching` | `false` | Enable Anthropic prompt-caching markers |
| `structuredMemory` | `false` | Enable Structured Memory |
| `dailyReflection` | `false` | Enable Daily Reflection |
| `enableStateDigest` | `true` | Enable working-state digests after observations |
| `enableTaskPlan` | `false` | Enable the automatic execution outline for ordinary Execute runs; suppressed during Plan-only and approved-plan execution |
| `enableS3` | enabled when `s3` exists | Master switch overriding S3 presence |

The service always binds `127.0.0.1:<port>`. Opening a firewall does not make LingClaw listen externally. Use a protected reverse proxy or SSH tunnel for remote access.

## Providers

A provider name is the custom key under `models.providers`. Use a short, stable, safe name and refer to models as `provider/model`.

Both `baseUrl` and `apiKey` are required in JSON; LingClaw does not fill in a missing base URL. An unauthenticated local endpoint such as the default Ollama server must still include `"apiKey": ""`. The `api` field may be omitted and defaults to `openai-completions`.

| `api` | Common base URL (must be explicit) | Upstream protocol |
|---|---|---|
| `openai-completions` | `https://api.openai.com/v1` | `POST /v1/chat/completions` |
| `openai-responses` | `https://api.openai.com/v1` | `POST /v1/responses` |
| `anthropic` | `https://api.anthropic.com` | Anthropic Messages |
| `gemini` | `https://generativelanguage.googleapis.com/v1beta` | Gemini generateContent |
| `ollama` | `http://127.0.0.1:11434` | Ollama chat |

`baseUrl` and `apiKey` accept an entire `${ENV_NAME}` placeholder. If it cannot be resolved, the provider is unavailable at runtime while its on-disk configuration remains intact.

```json
{
  "baseUrl": "${OPENAI_COMPAT_BASE_URL}",
  "apiKey": "${OPENAI_COMPAT_API_KEY}",
  "api": "openai-completions",
  "models": []
}
```

Settings → Models → Test sends a small real request to validate an endpoint and may consume a small number of tokens.

### Model fields

| Field | Description |
|---|---|
| `id` | Required upstream model ID |
| `name` | Display label; falls back to ID |
| `reasoning` | Whether reasoning effort is supported |
| `input` | `text` and optional `image` capabilities |
| `contextWindow` | Context window in tokens |
| `maxTokens` | Maximum output tokens |
| `cost` | Compatibility pricing metadata; Usage currently tracks tokens and does not calculate monetary cost |
| `compat` | Provider/model compatibility overrides |

`input: ["text", "image"]` is required before LingClaw exposes attachments or tool images, but it cannot guarantee every upstream combination is accepted. OpenAI-compatible Chat removes tool images and retries once only when the endpoint clearly rejects image/tool content. Authentication, rate-limit, and ordinary schema errors never trigger that fallback.

### Thinking compatibility

OpenAI-compatible models may set `compat.thinkingFormat`. The Settings selector provides:

- `openai`
- `qwen`
- `doubao`
- `deepseek-v4`
- `ollama`
- `gpt-oss`
- `ollama-gpt-oss`

Leaving it unset uses protocol defaults; custom legacy values remain preserved. `openai-responses` also accepts:

```json
{
  "compat": {
    "reasoning": {
      "summary": "auto"
    }
  }
}
```

Support for `off`, `minimal`, `xhigh`, and `max` differs by model. LingClaw maps requested levels to the configured dialect; `/status` shows the resolved model and think state.

## Agent model routing

```json
{
  "agents": {
    "defaults": {
      "model": {
        "primary": "openai/gpt-4o-mini",
        "fast": "openai/gpt-4o-mini",
        "sub-agent": "openai/gpt-4o-mini",
        "sub-agent-reviewer": "anthropic/claude-sonnet",
        "memory": "openai/gpt-4o-mini",
        "reflection": "openai/gpt-4o-mini",
        "context": "openai/gpt-4o-mini"
      }
    }
  }
}
```

- Settings → Agents → Switch all to updates every default role but not session `/model` overrides.
- `sub-agent-<name>` has priority over general `sub-agent`.
- Unset Fast, Memory, Reflection, and Context roles follow runtime fallback rules to the current effective model.
- If several providers contain the same plain model ID, use `provider/model` to remove ambiguity.
- Update agent roles and session overrides before removing or renaming a referenced model.

## MCP

`mcpServers` declares a discoverable server catalog only. A configured server does not automatically inject tools into every session. Enable the server and individual tools for the current session in Settings → MCP.

### stdio

```json
{
  "mcpServers": {
    "local-tools": {
      "transport": "stdio",
      "command": "npx",
      "args": ["-y", "example-mcp-server"],
      "env": {
        "EXAMPLE_TOKEN": "${EXAMPLE_TOKEN}"
      },
      "timeoutSecs": 30,
      "enabled": true
    }
  }
}
```

### Streamable HTTP

```json
{
  "mcpServers": {
    "remote-docs": {
      "transport": "streamable-http",
      "url": "https://example.com/mcp",
      "headers": {
        "Authorization": "Bearer ${MCP_TOKEN}"
      },
      "auth": {
        "clientId": "${MCP_CLIENT_ID}",
        "scopes": ["mcp:read"]
      },
      "enabled": true
    }
  }
}
```

Rules:

- `transport` supports `stdio` and `streamable-http`; when omitted, `command` or `url` determines it.
- A configured `cwd` must stay inside the current session workspace.
- `headers`, OAuth client fields, and stdio `env` accept `${ENV_NAME}`.
- OAuth tokens are stored in `~/.lingclaw/mcp-auth.json`, with refresh attempted after expiry.
- Session policy limits servers, tools, mutating-tool confirmation, and workspace-root exposure.
- Sub-agents inherit only enabled MCP tools that also pass their own tool policy.
- `lingclaw mcp-check` performs full diagnostics. `/mcp refresh` clears catalog caches, idle sessions, and failure cooldowns for the current workspace before rediscovery.

## S3-compatible image storage

```json
{
  "settings": {
    "enableS3": true
  },
  "s3": {
    "endpoint": "https://s3.us-east-1.amazonaws.com",
    "region": "us-east-1",
    "bucket": "my-bucket",
    "accessKey": "your-access-key",
    "secretKey": "your-secret-key",
    "prefix": "lingclaw/images/",
    "urlExpirySecs": 604800,
    "lifecycleDays": 14
  }
}
```

S3 serves user uploads and tool-image feedback:

- History stores only object key, configuration identity, name, and MIME, never signed URLs or raw Base64.
- When the configuration identity changes, old attachments are not reinterpreted in the new bucket.
- OpenAI and Anthropic consume signed URLs directly, so the endpoint must be reachable by the provider.
- Gemini and Ollama images are fetched by LingClaw and converted to `inlineData`/Base64, allowing private endpoints.
- Each user message and tool batch accepts up to 10 images, each at most 10MB, restricted to validated PNG/JPEG.
- `urlExpirySecs` controls the lifetime of signed URLs.
- `lifecycleDays` defaults to `14`. When greater than `0`, LingClaw reads the bucket lifecycle configuration at startup and after relevant Settings saves, then merges or updates an expiration rule for the configured `prefix`. Set it to `0` to disable this synchronization.

Use a dedicated bucket or prefix and grant only the required permissions. In addition to object read/write access, lifecycle synchronization requires `s3:GetLifecycleConfiguration` and `s3:PutLifecycleConfiguration`. A synchronization failure is logged as a warning and does not prevent LingClaw from starting.

S3 `accessKey` and `secretKey` values are currently read literally from JSON and do not support `${ENV_NAME}` placeholders. Restrict access to the configuration file and never commit real credentials.

## Environment variables

For most runtime settings, agent-role models, and legacy API Base/Key fields, precedence is: JSON configuration → corresponding environment variable → built-in default. `LINGCLAW_PROVIDER` and `LINGCLAW_ENABLE_S3` are exceptions: valid values override JSON. Provider `baseUrl` and `apiKey`, plus supported MCP fields, may use a complete `${ENV_NAME}` placeholder; this is expansion while reading JSON, not a general rule that an environment variable overrides an explicit JSON value.

| Variable | Description |
|---|---|
| `OPENAI_API_KEY` | Default OpenAI/OpenAI-compatible key |
| `ANTHROPIC_API_KEY` | Anthropic key |
| `OLLAMA_API_KEY` | Ollama key; usually empty locally |
| `GEMINI_API_KEY` / `GOOGLE_API_KEY` | Gemini key |
| `OPENAI_API_BASE` | OpenAI-compatible base URL |
| `OLLAMA_API_BASE` | Ollama base URL |
| `GEMINI_API_BASE` | Gemini base URL |
| `LINGCLAW_PROVIDER` | Legacy single-provider override |
| `LINGCLAW_MODEL` | Explicit primary model |
| `LINGCLAW_FAST_MODEL` | Fast model |
| `LINGCLAW_SUB_AGENT_MODEL` | Default sub-agent model |
| `LINGCLAW_MEMORY_MODEL` | Structured Memory model |
| `LINGCLAW_REFLECTION_MODEL` | Daily Reflection model |
| `LINGCLAW_CONTEXT_MODEL` | Context-compression model |
| `LINGCLAW_PORT` | Service port |
| `LINGCLAW_EXEC_TIMEOUT` | `exec` timeout in seconds |
| `LINGCLAW_TOOL_TIMEOUT` | Tool timeout in seconds |
| `LINGCLAW_SUB_AGENT_TIMEOUT` | Total sub-agent timeout |
| `LINGCLAW_MAX_LLM_RETRIES` | Transient LLM retry count |
| `LINGCLAW_MAX_CONTEXT_TOKENS` | Default context budget |
| `LINGCLAW_OPENAI_STREAM_INCLUDE_USAGE` | OpenAI stream-usage switch |
| `LINGCLAW_ANTHROPIC_PROMPT_CACHING` | Anthropic prompt-caching switch |
| `LINGCLAW_STRUCTURED_MEMORY` | Structured Memory switch |
| `LINGCLAW_DAILY_REFLECTION` | Daily Reflection switch |
| `LINGCLAW_ENABLE_S3` | S3 master switch, taking priority over JSON |

Boolean values accept `1/0`, `true/false`, `yes/no`, and `on/off`. Provider `${ENV_NAME}` syntax supports exact whole-string placeholders, not partial interpolation.

## Validation, backup, and troubleshooting

- Settings validates provider names, protocols, model IDs, agent references, MCP transport/cwd, and required S3 fields before saving.
- The configuration is saved through a temporary file and rename. Re-running the Setup Wizard backs up an existing file.
- When JSON syntax is invalid, Settings enters a repair state and never overwrites the file with defaults.
- A failed provider test does not save or replace a model automatically.
- An MCP server failure does not prevent LingClaw from starting. Inspect it in Settings, `/mcp`, or `lingclaw mcp-check`.
- Run `lingclaw doctor` to inspect Rust, Cargo, Git, Node/npm, source version, and installed version.

`.lingclaw.json` and `mcp-auth.json` may contain plaintext credentials. Restrict file permissions, never commit them, and remove real values from screenshots and issue reports.
