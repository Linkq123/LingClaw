use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::providers;

pub(crate) const DEFAULT_PORT: u16 = 18989;

// ── Config ──────────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provider {
    OpenAI,
    Anthropic,
    Ollama,
}

impl Provider {
    pub(crate) fn from_api_kind(api: &str) -> Self {
        match api.trim().to_ascii_lowercase().as_str() {
            "anthropic" => Self::Anthropic,
            "ollama" => Self::Ollama,
            _ => Self::OpenAI,
        }
    }

    pub(crate) fn default_api_base(self) -> &'static str {
        match self {
            Self::OpenAI => "https://api.openai.com/v1",
            Self::Anthropic => "https://api.anthropic.com",
            Self::Ollama => "http://127.0.0.1:11434",
        }
    }

    pub(crate) fn api_key_env_var(self) -> Option<&'static str> {
        match self {
            Self::OpenAI => Some("OPENAI_API_KEY"),
            Self::Anthropic => Some("ANTHROPIC_API_KEY"),
            Self::Ollama => None,
        }
    }

    pub(crate) fn detect(model: &str, api_base: &str, json_provider: Option<&str>) -> Self {
        // Explicit override: env var > JSON settings > auto-detect
        let env_explicit = std::env::var("LINGCLAW_PROVIDER")
            .unwrap_or_default()
            .to_lowercase();
        let explicit = if !env_explicit.is_empty() {
            env_explicit
        } else {
            json_provider.unwrap_or_default().to_lowercase()
        };
        if explicit == "anthropic" {
            return Self::Anthropic;
        }
        if explicit == "ollama" {
            return Self::Ollama;
        }
        if explicit == "openai" {
            return Self::OpenAI;
        }
        if let Some((provider_name, model_id)) = model.split_once('/') {
            let provider_name = provider_name.to_lowercase();
            if provider_name == "anthropic" {
                return Self::Anthropic;
            }
            if provider_name == "ollama" {
                return Self::Ollama;
            }
            if provider_name == "openai" {
                return Self::OpenAI;
            }
            if model_id.starts_with("claude") {
                return Self::Anthropic;
            }
        }
        // Auto-detect from model name or API base
        if model.starts_with("claude") || api_base.contains("anthropic.com") {
            Self::Anthropic
        } else if api_base.contains("11434") || api_base.contains("ollama") {
            Self::Ollama
        } else {
            Self::OpenAI
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
        }
    }
}

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) api_key: String,
    pub(crate) api_base: String,
    pub(crate) model: String,
    /// Optional lighter model for simple first-cycle queries.
    pub(crate) fast_model: Option<String>,
    /// Optional model for sub-agent task delegation.
    pub(crate) sub_agent_model: Option<String>,
    /// Optional model for structured memory extraction.
    pub(crate) memory_model: Option<String>,
    /// Optional model for post-execution reflection.
    pub(crate) reflection_model: Option<String>,
    /// Optional model for context compression (auto-compress hook).
    pub(crate) context_model: Option<String>,
    pub(crate) provider: Provider,
    pub(crate) openai_stream_include_usage: bool,
    pub(crate) anthropic_prompt_caching: bool,
    pub(crate) providers: HashMap<String, JsonProviderConfig>,
    pub(crate) mcp_servers: HashMap<String, JsonMcpServerConfig>,
    pub(crate) port: u16,
    pub(crate) max_context_tokens: usize,
    pub(crate) exec_timeout: Duration,
    pub(crate) tool_timeout: Duration,
    /// Total timeout for sub-agent execution (0 = unlimited, default: 300s).
    pub(crate) sub_agent_timeout: Duration,
    /// Maximum HTTP-level retries for transient LLM API errors (429, 5xx, connect/timeout).
    pub(crate) max_llm_retries: usize,
    pub(crate) max_output_bytes: usize,
    pub(crate) max_file_bytes: usize,
    /// Enable structured async memory (auto-extracts facts from conversations).
    pub(crate) structured_memory: bool,
    /// Enable post-execution daily reflection (writes to daily memory file).
    pub(crate) daily_reflection: bool,
    /// Optional S3-compatible storage for image uploads.
    pub(crate) s3: Option<S3Config>,
}

#[derive(Clone)]
pub(crate) struct S3Config {
    pub(crate) endpoint: String,
    pub(crate) region: String,
    pub(crate) bucket: String,
    pub(crate) access_key: String,
    pub(crate) secret_key: String,
    pub(crate) prefix: String,
    pub(crate) url_expiry_secs: u64,
    /// Bucket lifecycle retention in days for uploaded temp images (0 disables auto-management).
    pub(crate) lifecycle_days: u32,
}

pub(crate) fn format_sub_agent_timeout(timeout: Duration) -> String {
    if timeout.is_zero() {
        "unlimited".to_string()
    } else {
        format!("{}s", timeout.as_secs())
    }
}

fn trimmed_nonempty(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

pub(crate) fn parse_boolish_env(name: &str) -> Option<bool> {
    std::env::var(name)
        .ok()
        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        })
}

pub(crate) fn effective_enable_s3(
    settings_enable_s3: Option<bool>,
    env_enable_s3: Option<bool>,
) -> Option<bool> {
    env_enable_s3.or(settings_enable_s3)
}

pub(crate) fn normalized_s3_region(region: &str) -> String {
    let region = region.trim().to_ascii_lowercase();
    if region.is_empty() {
        "us-east-1".to_string()
    } else {
        region
    }
}

fn aws_s3_host_for_region(region: &str) -> String {
    let region = normalized_s3_region(region);
    let domain_suffix = if region.starts_with("cn-") {
        "amazonaws.com.cn"
    } else {
        "amazonaws.com"
    };
    format!("s3.{region}.{domain_suffix}")
}

fn is_official_aws_s3_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    host == "s3.amazonaws.com"
        || (host.starts_with("s3.")
            && (host.ends_with(".amazonaws.com") || host.ends_with(".amazonaws.com.cn")))
}

pub(crate) fn default_s3_endpoint(region: &str) -> String {
    format!("https://{}", aws_s3_host_for_region(region))
}

pub(crate) fn normalized_s3_endpoint(endpoint: Option<String>, region: &str) -> String {
    let endpoint = trimmed_nonempty(endpoint).unwrap_or_else(|| default_s3_endpoint(region));
    let trimmed = endpoint.trim_end_matches('/').to_string();

    if let Ok(mut parsed) = reqwest::Url::parse(&trimmed)
        && parsed.host_str().is_some_and(is_official_aws_s3_host)
    {
        let regional_host = aws_s3_host_for_region(region);
        if parsed.set_host(Some(&regional_host)).is_ok() {
            return parsed.to_string().trim_end_matches('/').to_string();
        }
    }

    trimmed
}

pub(crate) fn normalized_s3_prefix(prefix: Option<String>) -> String {
    let raw = prefix.unwrap_or_else(|| "lingclaw/images/".to_string());
    let normalized = raw.trim().trim_matches('/');
    if normalized.is_empty() {
        "lingclaw/images/".to_string()
    } else {
        format!("{normalized}/")
    }
}

fn align_runtime_provider_config(
    config: &mut Config,
    override_provider: bool,
    override_api_base: bool,
    override_api_key: bool,
) {
    if config.providers.is_empty() || !(override_provider || override_api_base || override_api_key)
    {
        return;
    }

    let resolved = config.resolve_model(&config.model);
    if override_provider {
        config.provider = resolved.provider;
    }
    if override_api_base {
        config.api_base = resolved.api_base.clone();
    }
    if override_api_key {
        config.api_key = resolved.api_key.clone();
    }
}

pub(crate) fn validate_provider_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Provider name cannot be empty.".to_string());
    }
    if name.contains('/') {
        return Err("Provider name cannot contain '/'.".to_string());
    }
    if name.chars().any(char::is_whitespace) {
        return Err("Provider name cannot contain whitespace.".to_string());
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
    {
        return Err(
            "Provider name may only contain letters, numbers, '.', '-' or '_'.".to_string(),
        );
    }
    Ok(())
}

fn is_builtin_provider_name(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "openai" | "anthropic" | "ollama"
    )
}

fn validate_provider_api_kind(api_kind: &str) -> Result<(), String> {
    if matches!(
        api_kind.trim().to_ascii_lowercase().as_str(),
        "openai-completions" | "anthropic" | "ollama"
    ) {
        Ok(())
    } else {
        Err(format!(
            "unsupported api '{api_kind}'. Expected one of: openai-completions, anthropic, ollama."
        ))
    }
}

fn validate_provider_entry(name: &str, provider: &JsonProviderConfig) -> Result<(), String> {
    validate_provider_name(name)
        .map_err(|error| format!("Invalid models.providers entry '{name}': {error}"))?;
    validate_provider_api_kind(&provider.api)
        .map_err(|error| format!("Invalid models.providers entry '{name}': {error}"))?;
    if provider.base_url.trim().is_empty() {
        return Err(format!(
            "Invalid models.providers entry '{name}': baseUrl cannot be empty."
        ));
    }
    Ok(())
}

fn validate_mcp_server_cwd(name: &str, cwd: &str, workspace: &Path) -> Result<(), String> {
    let workspace_root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let raw = Path::new(cwd);
    let relative = if raw.is_absolute() {
        if let Ok(relative) = raw.strip_prefix(workspace) {
            relative.to_path_buf()
        } else if let Ok(relative) = raw.strip_prefix(&workspace_root) {
            relative.to_path_buf()
        } else if let Ok(canonical_raw) = raw.canonicalize() {
            canonical_raw
                .strip_prefix(&workspace_root)
                .map(PathBuf::from)
                .map_err(|_| {
                    format!(
                        "Invalid mcpServers.{name}.cwd: path '{cwd}' is outside the session workspace '{}'",
                        workspace_root.display()
                    )
                })?
        } else {
            return Err(format!(
                "Invalid mcpServers.{name}.cwd: path '{cwd}' is outside the session workspace '{}'",
                workspace_root.display()
            ));
        }
    } else {
        raw.to_path_buf()
    };

    if relative.components().any(|component| {
        matches!(component, std::path::Component::Normal(part) if part == ".lingclaw-bootstrap")
    }) {
        return Err(format!(
            "Invalid mcpServers.{name}.cwd: path '{cwd}' targets protected internal workspace data."
        ));
    }

    let mut resolved = workspace_root.clone();
    for component in relative.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if resolved == workspace_root {
                    return Err(format!(
                        "Invalid mcpServers.{name}.cwd: path '{cwd}' is outside the session workspace '{}'",
                        workspace_root.display()
                    ));
                }
                resolved.pop();
            }
            std::path::Component::Normal(part) => {
                resolved.push(part);
                if let Ok(meta) = std::fs::symlink_metadata(&resolved)
                    && meta.file_type().is_symlink()
                {
                    return Err(format!(
                        "Invalid mcpServers.{name}.cwd: path '{cwd}' traverses symlink '{}' which is not permitted for security reasons.",
                        resolved.display()
                    ));
                }
            }
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                return Err(format!(
                    "Invalid mcpServers.{name}.cwd: path '{cwd}' is outside the session workspace '{}'",
                    workspace_root.display()
                ));
            }
        }
    }

    Ok(())
}

impl Config {
    pub(crate) fn load() -> Self {
        let json_cfg = sanitize_loaded_json_config(load_config_file());
        let settings = json_cfg.settings.unwrap_or_default();
        let settings_has_provider = settings.provider.is_some();
        let settings_has_api_key = settings.api_key.is_some();
        let settings_has_api_base = settings.api_base.is_some();
        let providers: HashMap<String, JsonProviderConfig> = json_cfg
            .models
            .and_then(|m| m.providers)
            .unwrap_or_default();
        let mcp_servers: HashMap<String, JsonMcpServerConfig> =
            json_cfg.mcp_servers.unwrap_or_default();

        // S3 config: gated by enableS3 setting (default: true when s3 section present).
        // Env var LINGCLAW_ENABLE_S3 overrides the JSON setting.
        let enable_s3 =
            effective_enable_s3(settings.enable_s3, parse_boolish_env("LINGCLAW_ENABLE_S3"));
        let s3 = if enable_s3 == Some(false) {
            None
        } else {
            json_cfg.s3.and_then(|j| {
                let region = normalized_s3_region(&trimmed_nonempty(j.region)?);
                let bucket = trimmed_nonempty(j.bucket)?;
                let access_key = trimmed_nonempty(j.access_key)?;
                let secret_key = trimmed_nonempty(j.secret_key)?;
                Some(S3Config {
                    endpoint: normalized_s3_endpoint(j.endpoint, &region),
                    region,
                    bucket,
                    access_key,
                    secret_key,
                    prefix: normalized_s3_prefix(j.prefix),
                    url_expiry_secs: j.url_expiry_secs.unwrap_or(604_800),
                    lifecycle_days: j.lifecycle_days.unwrap_or(14),
                })
            })
        };

        // Default model: JSON agents.defaults.model.primary → env LINGCLAW_MODEL → "gpt-4o-mini"
        let model_config = json_cfg
            .agents
            .and_then(|a| a.defaults)
            .and_then(|d| d.model);
        let default_from_json = model_config.as_ref().and_then(|m| m.primary.clone());
        let fast_model = model_config
            .as_ref()
            .and_then(|m| m.fast.clone())
            .or_else(|| std::env::var("LINGCLAW_FAST_MODEL").ok());
        let sub_agent_model = model_config
            .as_ref()
            .and_then(|m| m.sub_agent.clone())
            .or_else(|| std::env::var("LINGCLAW_SUB_AGENT_MODEL").ok());
        let memory_model = model_config
            .as_ref()
            .and_then(|m| m.memory.clone())
            .or_else(|| std::env::var("LINGCLAW_MEMORY_MODEL").ok());
        let reflection_model = model_config
            .as_ref()
            .and_then(|m| m.reflection.clone())
            .or_else(|| std::env::var("LINGCLAW_REFLECTION_MODEL").ok());
        let context_model = model_config
            .as_ref()
            .and_then(|m| m.context.clone())
            .or_else(|| std::env::var("LINGCLAW_CONTEXT_MODEL").ok());

        let model = default_from_json
            .or_else(|| std::env::var("LINGCLAW_MODEL").ok())
            .unwrap_or_else(|| "gpt-4o-mini".to_string());

        // API base: legacy settings.apiBase → env OPENAI_API_BASE → default
        let settings_api_base = settings.api_base.clone();
        let openai_api_base_env = std::env::var("OPENAI_API_BASE").ok();
        let ollama_api_base_env = std::env::var("OLLAMA_API_BASE").ok();
        let api_base_hint = settings_api_base
            .clone()
            .or_else(|| openai_api_base_env.clone())
            .or_else(|| ollama_api_base_env.clone())
            .unwrap_or_else(|| Provider::OpenAI.default_api_base().to_string());

        let provider = Provider::detect(&model, &api_base_hint, settings.provider.as_deref());

        // API key: legacy settings.apiKey → env vars → ""
        let api_key = settings.api_key.clone().unwrap_or_else(|| match provider {
            Provider::Anthropic => std::env::var("ANTHROPIC_API_KEY")
                .or_else(|_| std::env::var("OPENAI_API_KEY"))
                .unwrap_or_default(),
            Provider::OpenAI => std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            Provider::Ollama => std::env::var("OLLAMA_API_KEY")
                .or_else(|_| std::env::var("OPENAI_API_KEY"))
                .unwrap_or_default(),
        });

        let api_base = if let Some(explicit) = settings_api_base {
            explicit
        } else {
            match provider {
                Provider::OpenAI => openai_api_base_env
                    .unwrap_or_else(|| Provider::OpenAI.default_api_base().to_string()),
                Provider::Anthropic => match openai_api_base_env {
                    Some(base) if base != Provider::OpenAI.default_api_base() => base,
                    _ => Provider::Anthropic.default_api_base().to_string(),
                },
                Provider::Ollama => ollama_api_base_env
                    .unwrap_or_else(|| Provider::Ollama.default_api_base().to_string()),
            }
        };

        let mut config = Self {
            api_key,
            api_base,
            model,
            fast_model,
            sub_agent_model,
            memory_model,
            reflection_model,
            context_model,
            provider,
            openai_stream_include_usage: settings
                .openai_stream_include_usage
                .or_else(|| {
                    std::env::var("LINGCLAW_OPENAI_STREAM_INCLUDE_USAGE")
                        .ok()
                        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
                            "1" | "true" | "yes" | "on" => Some(true),
                            "0" | "false" | "no" | "off" => Some(false),
                            _ => None,
                        })
                })
                .unwrap_or(false),
            anthropic_prompt_caching: settings
                .anthropic_prompt_caching
                .or_else(|| {
                    std::env::var("LINGCLAW_ANTHROPIC_PROMPT_CACHING")
                        .ok()
                        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
                            "1" | "true" | "yes" | "on" => Some(true),
                            "0" | "false" | "no" | "off" => Some(false),
                            _ => None,
                        })
                })
                .unwrap_or(false),
            providers,
            mcp_servers,
            port: settings
                .port
                .or_else(|| std::env::var("LINGCLAW_PORT").ok()?.parse().ok())
                .unwrap_or(DEFAULT_PORT),
            max_context_tokens: settings
                .max_context_tokens
                .or_else(|| {
                    std::env::var("LINGCLAW_MAX_CONTEXT_TOKENS")
                        .ok()?
                        .parse()
                        .ok()
                })
                .unwrap_or(32000),
            exec_timeout: Duration::from_secs(
                settings
                    .exec_timeout
                    .or_else(|| std::env::var("LINGCLAW_EXEC_TIMEOUT").ok()?.parse().ok())
                    .unwrap_or(30),
            ),
            tool_timeout: Duration::from_secs(
                settings
                    .tool_timeout
                    .or_else(|| std::env::var("LINGCLAW_TOOL_TIMEOUT").ok()?.parse().ok())
                    .unwrap_or(30),
            ),
            sub_agent_timeout: Duration::from_secs(
                settings
                    .sub_agent_timeout
                    .or_else(|| {
                        std::env::var("LINGCLAW_SUB_AGENT_TIMEOUT")
                            .ok()?
                            .parse()
                            .ok()
                    })
                    .unwrap_or(300),
            ),
            max_llm_retries: settings
                .max_llm_retries
                .or_else(|| std::env::var("LINGCLAW_MAX_LLM_RETRIES").ok()?.parse().ok())
                .unwrap_or(2)
                .min(10),
            max_output_bytes: settings.max_output_bytes.unwrap_or(50 * 1024),
            max_file_bytes: settings.max_file_bytes.unwrap_or(200 * 1024),
            structured_memory: settings
                .structured_memory
                .or_else(|| {
                    std::env::var("LINGCLAW_STRUCTURED_MEMORY")
                        .ok()
                        .and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
                            "1" | "true" | "yes" | "on" => Some(true),
                            "0" | "false" | "no" | "off" => Some(false),
                            _ => None,
                        })
                })
                .unwrap_or(false),
            daily_reflection: settings
                .daily_reflection
                .or_else(|| parse_boolish_env("LINGCLAW_DAILY_REFLECTION"))
                .unwrap_or(false),
            s3,
        };
        align_runtime_provider_config(
            &mut config,
            !settings_has_provider,
            !settings_has_api_base,
            !settings_has_api_key,
        );
        config
    }

    pub(crate) fn validate_json_mcp_servers(json_cfg: &JsonConfig) -> Result<(), String> {
        let Some(servers) = json_cfg.mcp_servers.as_ref() else {
            return Ok(());
        };

        for (name, server) in servers {
            if server.command.trim().is_empty() {
                return Err(format!(
                    "Invalid mcpServers.{name}: command cannot be empty."
                ));
            }
            if server.timeout_secs == Some(0) {
                return Err(format!(
                    "Invalid mcpServers.{name}: timeoutSecs must be greater than 0."
                ));
            }
        }

        Ok(())
    }

    pub(crate) fn validate_json_mcp_servers_for_workspace(
        json_cfg: &JsonConfig,
        workspace: &Path,
    ) -> Result<(), String> {
        Self::validate_json_mcp_servers(json_cfg)?;

        let Some(servers) = json_cfg.mcp_servers.as_ref() else {
            return Ok(());
        };

        for (name, server) in servers {
            let Some(cwd) = server
                .cwd
                .as_deref()
                .map(str::trim)
                .filter(|cwd| !cwd.is_empty())
            else {
                continue;
            };
            validate_mcp_server_cwd(name, cwd, workspace)?;
        }

        Ok(())
    }

    pub(crate) fn memory_model_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.memory_model.as_deref().unwrap_or(fallback)
    }

    pub(crate) fn reflection_model_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.reflection_model
            .as_deref()
            .or(self.memory_model.as_deref())
            .unwrap_or(fallback)
    }

    pub(crate) fn context_model_or<'a>(&'a self, fallback: &'a str) -> &'a str {
        self.context_model.as_deref().unwrap_or(fallback)
    }

    /// Resolve a model reference ("provider/model" or plain "model-name") to
    /// a concrete provider, API base, API key, and model ID.
    pub(crate) fn resolve_model(&self, model_ref: &str) -> providers::ResolvedModel {
        let fallback_resolved = |provider: Provider, model_id: &str| providers::ResolvedModel {
            provider,
            api_base: match provider {
                Provider::Anthropic
                    if self.provider != Provider::Anthropic
                        || self.api_base == Provider::OpenAI.default_api_base() =>
                {
                    if self.api_base == Provider::OpenAI.default_api_base() {
                        Provider::Anthropic.default_api_base().to_string()
                    } else {
                        self.api_base.clone()
                    }
                }
                Provider::Ollama
                    if self.provider != Provider::Ollama
                        || self.api_base == Provider::OpenAI.default_api_base() =>
                {
                    std::env::var("OLLAMA_API_BASE")
                        .unwrap_or_else(|_| Provider::Ollama.default_api_base().to_string())
                }
                _ => self.api_base.clone(),
            },
            api_key: match provider {
                Provider::Anthropic if self.provider != Provider::Anthropic => {
                    std::env::var("ANTHROPIC_API_KEY")
                        .or_else(|_| std::env::var("OPENAI_API_KEY"))
                        .unwrap_or_else(|_| self.api_key.clone())
                }
                Provider::OpenAI if self.provider != Provider::OpenAI => {
                    std::env::var("OPENAI_API_KEY").unwrap_or_else(|_| self.api_key.clone())
                }
                Provider::Ollama if self.provider != Provider::Ollama => {
                    std::env::var("OLLAMA_API_KEY")
                        .or_else(|_| std::env::var("OPENAI_API_KEY"))
                        .unwrap_or_else(|_| self.api_key.clone())
                }
                _ => self.api_key.clone(),
            },
            model_id: model_id.to_string(),
            reasoning: false,
            thinking_format: None,
            max_tokens: None,
            context_window: self.max_context_tokens as u64,
            stream_include_usage: self.openai_stream_include_usage,
            anthropic_prompt_caching: self.anthropic_prompt_caching,
        };

        let build_resolved =
            |pc: &JsonProviderConfig, model_id: &str, entry: Option<&JsonModelEntry>| {
                let reasoning = entry.and_then(|e| e.reasoning).unwrap_or(false);
                let thinking_format = entry
                    .and_then(|e| e.compat.as_ref())
                    .and_then(|c| c.get("thinkingFormat"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let max_tokens = entry.and_then(|e| e.max_tokens);
                let context_window = entry
                    .and_then(|e| e.context_window)
                    .unwrap_or(self.max_context_tokens as u64);
                providers::ResolvedModel {
                    provider: Provider::from_api_kind(&pc.api),
                    api_base: pc.base_url.clone(),
                    api_key: pc.api_key.clone(),
                    model_id: model_id.to_string(),
                    reasoning,
                    thinking_format,
                    max_tokens,
                    context_window,
                    stream_include_usage: self.openai_stream_include_usage,
                    anthropic_prompt_caching: self.anthropic_prompt_caching,
                }
            };

        // Try "provider/model" format
        if let Some((prov_name, model_id)) = model_ref.split_once('/') {
            if let Some(pc) = self.providers.get(prov_name) {
                let entry = pc.models.iter().find(|m| m.id == model_id);
                return build_resolved(pc, model_id, entry);
            }
            if self.providers.is_empty() {
                let provider = match prov_name.to_ascii_lowercase().as_str() {
                    "anthropic" => Some(Provider::Anthropic),
                    "openai" => Some(Provider::OpenAI),
                    "ollama" => Some(Provider::Ollama),
                    _ => None,
                };
                if let Some(provider) = provider {
                    return fallback_resolved(provider, model_id);
                }
            }
        }
        // Fallback: search configured providers by plain model id, preferring
        // an exact match to the current runtime config, then same provider
        // type, all with stable provider-name ordering.
        let mut provider_names: Vec<&str> =
            self.providers.keys().map(|name| name.as_str()).collect();
        provider_names.sort_unstable_by(|left, right| {
            let rank = |name: &str| {
                let Some(pc) = self.providers.get(name) else {
                    return 3_u8;
                };
                let pc_provider = Provider::from_api_kind(&pc.api);
                if pc_provider == self.provider
                    && pc.base_url == self.api_base
                    && pc.api_key == self.api_key
                {
                    0_u8
                } else if pc_provider == self.provider && pc.base_url == self.api_base {
                    1_u8
                } else if pc_provider == self.provider {
                    2_u8
                } else {
                    3_u8
                }
            };

            rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
        });

        for name in &provider_names {
            let Some(pc) = self.providers.get(*name) else {
                continue;
            };
            if let Some(entry) = pc.models.iter().find(|m| m.id == model_ref) {
                return build_resolved(pc, model_ref, Some(entry));
            }
        }

        // Fallback to env-based config
        fallback_resolved(self.provider, model_ref)
    }

    pub(crate) fn resolve_provider_name(&self, model_ref: &str) -> String {
        if let Some((prov_name, _)) = model_ref.split_once('/') {
            if self.providers.contains_key(prov_name) {
                return prov_name.to_string();
            }
            if self.providers.is_empty() && is_builtin_provider_name(prov_name) {
                return prov_name.to_ascii_lowercase();
            }
        }

        let resolved_ref = self.resolved_model_ref(model_ref);
        if let Some((prov_name, _)) = resolved_ref.split_once('/') {
            if self.providers.contains_key(prov_name) {
                return prov_name.to_string();
            }
            if self.providers.is_empty() && is_builtin_provider_name(prov_name) {
                return prov_name.to_ascii_lowercase();
            }
        }

        self.resolve_model(model_ref).provider.label().to_string()
    }

    /// List all available models: from config file providers + the default env model.
    pub(crate) fn available_models(&self) -> Vec<String> {
        let mut models: Vec<String> = Vec::new();
        for (prov_name, pc) in &self.providers {
            for m in &pc.models {
                models.push(format!("{prov_name}/{}", m.id));
            }
        }
        if models.is_empty() {
            models.push(self.model.clone());
        } else if let Ok(canonical) = self.canonical_model_ref(&self.model)
            && !models.iter().any(|m| m == &canonical)
        {
            models.push(canonical);
        }
        models
    }

    pub(crate) fn resolved_model_ref(&self, model_ref: &str) -> String {
        if let Some((prov_name, model_id)) = model_ref.split_once('/') {
            if self.providers.contains_key(prov_name) {
                return format!("{prov_name}/{model_id}");
            }
            if self.providers.is_empty() {
                let provider = prov_name.to_ascii_lowercase();
                if provider == "openai" || provider == "anthropic" || provider == "ollama" {
                    return format!("{provider}/{model_id}");
                }
            }
        }

        let mut provider_names: Vec<&str> =
            self.providers.keys().map(|name| name.as_str()).collect();
        provider_names.sort_unstable_by(|left, right| {
            let rank = |name: &str| {
                let Some(pc) = self.providers.get(name) else {
                    return 3_u8;
                };
                let pc_provider = Provider::from_api_kind(&pc.api);
                if pc_provider == self.provider
                    && pc.base_url == self.api_base
                    && pc.api_key == self.api_key
                {
                    0_u8
                } else if pc_provider == self.provider && pc.base_url == self.api_base {
                    1_u8
                } else if pc_provider == self.provider {
                    2_u8
                } else {
                    3_u8
                }
            };

            rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
        });

        for name in &provider_names {
            let Some(pc) = self.providers.get(*name) else {
                continue;
            };
            if pc.models.iter().any(|m| m.id == model_ref) {
                return format!("{name}/{model_ref}");
            }
        }

        model_ref.to_string()
    }

    pub(crate) fn canonical_model_ref(&self, model_ref: &str) -> Result<String, String> {
        let trimmed = model_ref.trim();
        if trimmed.is_empty() {
            return Err("Model name cannot be empty.".into());
        }

        if let Some((prov_name, model_id)) = trimmed.split_once('/') {
            if self.providers.is_empty() {
                let provider = prov_name.to_ascii_lowercase();
                if provider == "openai" || provider == "anthropic" || provider == "ollama" {
                    return Ok(format!("{provider}/{model_id}"));
                }
                return Err(format!(
                    "Unknown provider '{prov_name}'. Use 'openai', 'anthropic', or 'ollama'."
                ));
            }
            let Some(pc) = self.providers.get(prov_name) else {
                return Err(format!(
                    "Unknown provider '{prov_name}'. Use /model to list available models."
                ));
            };
            if pc.models.iter().any(|m| m.id == model_id) {
                return Ok(format!("{prov_name}/{model_id}"));
            }
            return Err(format!(
                "Model '{model_id}' is not configured under provider '{prov_name}'."
            ));
        }

        let matches: Vec<String> = self
            .providers
            .iter()
            .filter(|(_, pc)| pc.models.iter().any(|m| m.id == trimmed))
            .map(|(prov_name, _)| format!("{prov_name}/{trimmed}"))
            .collect();

        match matches.len() {
            0 if self.providers.is_empty() => Ok(trimmed.to_string()),
            0 => Err(format!(
                "Unknown model '{trimmed}'. Use /model to list available models."
            )),
            1 => Ok(matches[0].clone()),
            _ => Err(format!(
                "Model '{trimmed}' is ambiguous. Use one of: {}",
                matches.join(", ")
            )),
        }
    }

    /// Look up the JsonModelEntry for a given model reference ("provider/model" or plain id).
    pub(crate) fn find_model_entry(&self, model_ref: &str) -> Option<&JsonModelEntry> {
        if let Some((prov_name, model_id)) = model_ref.split_once('/')
            && let Some(pc) = self.providers.get(prov_name)
        {
            return pc.models.iter().find(|m| m.id == model_id);
        }
        // Fallback: search all providers by plain id
        for pc in self.providers.values() {
            if let Some(entry) = pc.models.iter().find(|m| m.id == model_ref) {
                return Some(entry);
            }
        }
        None
    }

    /// Return the effective context token limit for the given model.
    /// Priority: model's contextWindow → settings.maxContextTokens → 32000.
    pub(crate) fn context_limit_for_model(&self, model_ref: &str) -> usize {
        if let Some(entry) = self.find_model_entry(model_ref)
            && let Some(cw) = entry.context_window
        {
            return cw as usize;
        }
        self.max_context_tokens
    }

    /// Check if a model supports image input based on its config `input` array.
    pub(crate) fn model_supports_image(&self, model_ref: &str) -> bool {
        self.find_model_entry(model_ref)
            .and_then(|e| e.input.as_ref())
            .is_some_and(|inputs| inputs.iter().any(|i| i == "image"))
    }
}

fn sanitize_loaded_json_config(json_cfg: JsonConfig) -> JsonConfig {
    let mut json_cfg = json_cfg;

    let mut providers = json_cfg
        .models
        .as_ref()
        .and_then(|models| models.providers.as_ref())
        .cloned()
        .unwrap_or_default();

    providers.retain(
        |name, provider| match validate_provider_entry(name, provider) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("WARNING: {error}");
                false
            }
        },
    );

    for (name, provider) in providers.iter_mut() {
        provider.models.retain(|model| {
            if model.id.trim().is_empty() {
                eprintln!(
                    "WARNING: Ignoring invalid models.providers.{name}.models entry with empty id."
                );
                false
            } else {
                true
            }
        });
    }

    if let Some(models) = json_cfg.models.as_mut() {
        models.providers = if providers.is_empty() {
            None
        } else {
            Some(providers.clone())
        };
    }

    if let Some(servers) = json_cfg.mcp_servers.as_mut() {
        servers.retain(|name, server| {
            if server.command.trim().is_empty() {
                eprintln!("WARNING: Ignoring invalid mcpServers.{name}: command cannot be empty.");
                false
            } else if server.timeout_secs == Some(0) {
                eprintln!(
                    "WARNING: Ignoring invalid mcpServers.{name}.timeoutSecs: 0 is not allowed; using default timeout instead."
                );
                server.timeout_secs = None;
                true
            } else {
                true
            }
        });
        if servers.is_empty() {
            json_cfg.mcp_servers = None;
        }
    }

    if let Some(model_defaults) = json_cfg
        .agents
        .as_mut()
        .and_then(|agents| agents.defaults.as_mut())
        .and_then(|defaults| defaults.model.as_mut())
    {
        for (field, model_ref) in [
            ("primary", &mut model_defaults.primary),
            ("fast", &mut model_defaults.fast),
            ("sub-agent", &mut model_defaults.sub_agent),
            ("memory", &mut model_defaults.memory),
            ("reflection", &mut model_defaults.reflection),
            ("context", &mut model_defaults.context),
        ] {
            let Some(value) = model_ref.as_deref() else {
                continue;
            };
            if let Err(error) = validate_agent_model_ref(field, value, &providers) {
                eprintln!("WARNING: Ignoring invalid config file entry: {error}");
                *model_ref = None;
            }
        }
    }

    json_cfg
}

// ── Config File (lingclaw.json) ──────────────────────────────────────────────

#[derive(Deserialize, Default)]
pub(crate) struct JsonConfig {
    pub(crate) settings: Option<JsonSettings>,
    pub(crate) models: Option<JsonModelsConfig>,
    pub(crate) agents: Option<JsonAgentsConfig>,
    #[serde(rename = "mcpServers")]
    pub(crate) mcp_servers: Option<HashMap<String, JsonMcpServerConfig>>,
    pub(crate) s3: Option<JsonS3Config>,
}

#[derive(Deserialize, Default)]
pub(crate) struct JsonSettings {
    pub(crate) port: Option<u16>,
    pub(crate) provider: Option<String>,
    #[serde(rename = "openaiStreamIncludeUsage")]
    pub(crate) openai_stream_include_usage: Option<bool>,
    #[serde(rename = "anthropicPromptCaching")]
    pub(crate) anthropic_prompt_caching: Option<bool>,
    #[serde(rename = "apiKey")]
    pub(crate) api_key: Option<String>,
    #[serde(rename = "apiBase")]
    pub(crate) api_base: Option<String>,
    #[serde(rename = "execTimeout")]
    pub(crate) exec_timeout: Option<u64>,
    #[serde(rename = "toolTimeout")]
    pub(crate) tool_timeout: Option<u64>,
    /// Total timeout for sub-agent execution in seconds (0 = unlimited, default: 300).
    #[serde(rename = "subAgentTimeout")]
    pub(crate) sub_agent_timeout: Option<u64>,
    /// Maximum HTTP-level retries for transient LLM API errors (default: 2).
    #[serde(rename = "maxLlmRetries")]
    pub(crate) max_llm_retries: Option<usize>,
    #[serde(rename = "maxContextTokens")]
    pub(crate) max_context_tokens: Option<usize>,
    #[serde(rename = "maxOutputBytes")]
    pub(crate) max_output_bytes: Option<usize>,
    #[serde(rename = "maxFileBytes")]
    pub(crate) max_file_bytes: Option<usize>,
    /// Enable structured async memory (default: false).
    #[serde(rename = "structuredMemory")]
    pub(crate) structured_memory: Option<bool>,
    /// Enable post-execution daily reflection (default: false).
    #[serde(rename = "dailyReflection")]
    pub(crate) daily_reflection: Option<bool>,
    /// Enable S3-compatible image upload (default: true when s3 section is configured).
    #[serde(rename = "enableS3")]
    pub(crate) enable_s3: Option<bool>,
}

#[derive(Deserialize, Default)]
pub(crate) struct JsonModelsConfig {
    pub(crate) providers: Option<HashMap<String, JsonProviderConfig>>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct JsonProviderConfig {
    #[serde(rename = "baseUrl")]
    pub(crate) base_url: String,
    #[serde(rename = "apiKey")]
    pub(crate) api_key: String,
    #[serde(default = "default_api_protocol")]
    pub(crate) api: String,
    #[serde(default)]
    pub(crate) models: Vec<JsonModelEntry>,
}

fn default_api_protocol() -> String {
    "openai-completions".to_string()
}

fn default_mcp_enabled() -> bool {
    true
}

#[derive(Deserialize, Serialize, Clone, Default)]
pub(crate) struct JsonMcpServerConfig {
    pub(crate) command: String,
    #[serde(default)]
    pub(crate) args: Vec<String>,
    #[serde(default)]
    pub(crate) env: HashMap<String, String>,
    #[serde(default)]
    pub(crate) cwd: Option<String>,
    #[serde(default = "default_mcp_enabled")]
    pub(crate) enabled: bool,
    #[serde(rename = "timeoutSecs")]
    pub(crate) timeout_secs: Option<u64>,
}

#[derive(Deserialize, Serialize, Clone, Default)]
pub(crate) struct JsonModelEntry {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) cost: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "contextWindow")]
    pub(crate) context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxTokens")]
    pub(crate) max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) compat: Option<serde_json::Value>,
}

pub(crate) fn validate_json_provider_names(json_cfg: &JsonConfig) -> Result<(), String> {
    let Some(providers) = json_cfg
        .models
        .as_ref()
        .and_then(|models| models.providers.as_ref())
    else {
        return Ok(());
    };

    for (name, provider) in providers {
        validate_provider_entry(name, provider)?;
    }

    Ok(())
}

pub(crate) fn validate_json_agent_model_refs(json_cfg: &JsonConfig) -> Result<(), String> {
    let providers = json_cfg
        .models
        .as_ref()
        .and_then(|models| models.providers.as_ref())
        .cloned()
        .unwrap_or_default();

    let Some(model_defaults) = json_cfg
        .agents
        .as_ref()
        .and_then(|agents| agents.defaults.as_ref())
        .and_then(|defaults| defaults.model.as_ref())
    else {
        return Ok(());
    };

    for (field, model_ref) in [
        ("primary", model_defaults.primary.as_deref()),
        ("fast", model_defaults.fast.as_deref()),
        ("sub-agent", model_defaults.sub_agent.as_deref()),
        ("memory", model_defaults.memory.as_deref()),
        ("reflection", model_defaults.reflection.as_deref()),
        ("context", model_defaults.context.as_deref()),
    ] {
        let Some(model_ref) = model_ref else {
            continue;
        };
        validate_agent_model_ref(field, model_ref, &providers)?;
    }

    Ok(())
}

fn validate_agent_model_ref(
    field: &str,
    model_ref: &str,
    providers: &HashMap<String, JsonProviderConfig>,
) -> Result<(), String> {
    let has_configured_providers = !providers.is_empty();
    let Some((provider_name, model_id)) = model_ref.split_once('/') else {
        return Ok(());
    };
    if model_id.trim().is_empty() {
        return Err(format!(
            "Invalid agents.defaults.model.{field}: model id cannot be empty."
        ));
    }
    if has_configured_providers && providers.contains_key(provider_name) {
        if let Some(provider_cfg) = providers.get(provider_name)
            && !provider_cfg.models.is_empty()
            && !provider_cfg.models.iter().any(|model| model.id == model_id)
        {
            return Err(format!(
                "Invalid agents.defaults.model.{field}: unknown model '{model_id}' for provider '{provider_name}'. Add it in models.providers.{provider_name}.models first."
            ));
        }
        return Ok(());
    }
    if !has_configured_providers && is_builtin_provider_name(provider_name) {
        return Ok(());
    }
    Err(format!(
        "Invalid agents.defaults.model.{field}: unknown provider '{provider_name}'. Add it in models.providers first."
    ))
}

pub(crate) fn validate_json_provider_models(json_cfg: &JsonConfig) -> Result<(), String> {
    let Some(providers) = json_cfg
        .models
        .as_ref()
        .and_then(|models| models.providers.as_ref())
    else {
        return Ok(());
    };

    for (name, provider) in providers {
        for (index, model) in provider.models.iter().enumerate() {
            if model.id.trim().is_empty() {
                return Err(format!(
                    "Invalid models.providers.{name}.models[{index}].id: model id cannot be empty."
                ));
            }
        }
    }

    Ok(())
}

#[derive(Deserialize, Clone)]
pub(crate) struct JsonS3Config {
    pub(crate) endpoint: Option<String>,
    pub(crate) region: Option<String>,
    pub(crate) bucket: Option<String>,
    #[serde(rename = "accessKey")]
    pub(crate) access_key: Option<String>,
    #[serde(rename = "secretKey")]
    pub(crate) secret_key: Option<String>,
    pub(crate) prefix: Option<String>,
    /// Presigned URL expiry in seconds (default: 604800 = 7 days for AWS compatibility).
    #[serde(rename = "urlExpirySecs")]
    pub(crate) url_expiry_secs: Option<u64>,
    /// Temp image lifecycle retention in days (default: 14, 0 disables auto-management).
    #[serde(rename = "lifecycleDays")]
    pub(crate) lifecycle_days: Option<u32>,
}

#[derive(Deserialize, Default)]
pub(crate) struct JsonAgentsConfig {
    pub(crate) defaults: Option<JsonAgentDefaults>,
}

#[derive(Deserialize, Default)]
pub(crate) struct JsonAgentDefaults {
    pub(crate) model: Option<JsonDefaultModel>,
}

#[derive(Deserialize, Default)]
pub(crate) struct JsonDefaultModel {
    pub(crate) primary: Option<String>,
    /// Optional lighter/cheaper model for simple queries (cycle 0, short input).
    pub(crate) fast: Option<String>,
    /// Optional model for sub-agent task delegation.
    #[serde(rename = "sub-agent")]
    pub(crate) sub_agent: Option<String>,
    /// Optional model for structured memory extraction.
    pub(crate) memory: Option<String>,
    /// Optional model for post-execution reflection.
    pub(crate) reflection: Option<String>,
    /// Optional model for context compression.
    pub(crate) context: Option<String>,
}

pub(crate) fn config_dir_path() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    if home.is_empty() {
        return None;
    }
    Some(Path::new(&home).join(".lingclaw"))
}

pub(crate) fn config_file_path() -> Option<PathBuf> {
    Some(config_dir_path()?.join(".lingclaw.json"))
}

pub(crate) fn load_config_file() -> JsonConfig {
    let path = match config_file_path() {
        Some(p) => p,
        None => return JsonConfig::default(),
    };
    match std::fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            eprintln!("WARNING: Failed to parse {}: {e}", path.display());
            JsonConfig::default()
        }),
        Err(_) => JsonConfig::default(),
    }
}

#[cfg(test)]
#[path = "tests/config_tests.rs"]
mod config_tests;
