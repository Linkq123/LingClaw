use axum::{
    extract::{
        ws::{Message as WsMsg, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use futures::{stream::SplitSink, SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;
use tower_http::{cors::CorsLayer, services::ServeDir};

mod prompts;
mod providers;
mod tools;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// ══════════════════════════════════════════════════════════════════════════════
//  Config
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Provider {
    OpenAI,
    Anthropic,
}

impl Provider {
    fn detect(model: &str, api_base: &str, json_provider: Option<&str>) -> Self {
        // Explicit override: env var > JSON settings > auto-detect
        let env_explicit = std::env::var("LINGCLAW_PROVIDER").unwrap_or_default().to_lowercase();
        let explicit = if !env_explicit.is_empty() {
            env_explicit
        } else {
            json_provider.unwrap_or_default().to_lowercase()
        };
        if explicit == "anthropic" {
            return Self::Anthropic;
        }
        if explicit == "openai" {
            return Self::OpenAI;
        }
        // Auto-detect from model name or API base
        if model.starts_with("claude")
            || api_base.contains("anthropic.com")
        {
            Self::Anthropic
        } else {
            Self::OpenAI
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::OpenAI => "openai",
            Self::Anthropic => "anthropic",
        }
    }
}

struct Config {
    api_key: String,
    api_base: String,
    model: String,
    provider: Provider,
    providers: HashMap<String, JsonProviderConfig>,
    port: u16,
    max_context_tokens: usize,
    exec_timeout: Duration,
    max_tool_rounds: usize,
    max_output_bytes: usize,
    max_file_bytes: usize,
}

impl Config {
    fn load() -> Self {
        let json_cfg = load_config_file();
        let settings = json_cfg.settings.unwrap_or_default();
        let providers: HashMap<String, JsonProviderConfig> = json_cfg
            .models
            .and_then(|m| m.providers)
            .unwrap_or_default();

        // Default model: JSON agents.defaults.model.primary → env LINGCLAW_MODEL → "gpt-4o-mini"
        let default_from_json = json_cfg
            .agents
            .and_then(|a| a.defaults)
            .and_then(|d| d.model)
            .and_then(|m| m.primary);

        let model = default_from_json
            .or_else(|| std::env::var("LINGCLAW_MODEL").ok())
            .unwrap_or_else(|| "gpt-4o-mini".to_string());

        // API base: JSON settings.apiBase → env OPENAI_API_BASE → default
        let api_base = settings
            .api_base
            .clone()
            .or_else(|| std::env::var("OPENAI_API_BASE").ok())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        let provider = Provider::detect(&model, &api_base, settings.provider.as_deref());

        // API key: JSON settings.apiKey → env vars → ""
        let api_key = settings.api_key.clone().unwrap_or_else(|| match provider {
            Provider::Anthropic => {
                std::env::var("ANTHROPIC_API_KEY")
                    .or_else(|_| std::env::var("OPENAI_API_KEY"))
                    .unwrap_or_default()
            }
            Provider::OpenAI => std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        });

        // Adjust api_base for Anthropic when still on default OpenAI URL
        let api_base = match provider {
            Provider::Anthropic => {
                if api_base == "https://api.openai.com/v1" {
                    "https://api.anthropic.com".to_string()
                } else {
                    api_base
                }
            }
            Provider::OpenAI => api_base,
        };

        Self {
            api_key,
            api_base,
            model,
            provider,
            providers,
            port: settings.port
                .or_else(|| std::env::var("LINGCLAW_PORT").ok()?.parse().ok())
                .unwrap_or(3000),
            max_context_tokens: settings.max_context_tokens
                .or_else(|| std::env::var("LINGCLAW_MAX_CONTEXT_TOKENS").ok()?.parse().ok())
                .unwrap_or(32000),
            exec_timeout: Duration::from_secs(
                settings.exec_timeout
                    .or_else(|| std::env::var("LINGCLAW_EXEC_TIMEOUT").ok()?.parse().ok())
                    .unwrap_or(30),
            ),
            max_tool_rounds: settings.max_tool_rounds
                .or_else(|| std::env::var("LINGCLAW_MAX_TOOL_ROUNDS").ok()?.parse().ok())
                .unwrap_or(20),
            max_output_bytes: settings.max_output_bytes.unwrap_or(50 * 1024),
            max_file_bytes: settings.max_file_bytes.unwrap_or(200 * 1024),
        }
    }

    /// Resolve a model reference ("provider/model" or plain "model-name") to
    /// a concrete provider, API base, API key, and model ID.
    fn resolve_model(&self, model_ref: &str) -> providers::ResolvedModel {
        // Try "provider/model" format
        if let Some((prov_name, model_id)) = model_ref.split_once('/') {
            if let Some(pc) = self.providers.get(prov_name) {
                return providers::ResolvedModel {
                    provider: match pc.api.as_str() {
                        "anthropic" => Provider::Anthropic,
                        _ => Provider::OpenAI,
                    },
                    api_base: pc.base_url.clone(),
                    api_key: pc.api_key.clone(),
                    model_id: model_id.to_string(),
                };
            }
        }
        // Fallback to env-based config
        providers::ResolvedModel {
            provider: self.provider,
            api_base: self.api_base.clone(),
            api_key: self.api_key.clone(),
            model_id: model_ref.to_string(),
        }
    }

    /// List all available models: from config file providers + the default env model.
    fn available_models(&self) -> Vec<String> {
        let mut models: Vec<String> = Vec::new();
        for (prov_name, pc) in &self.providers {
            for m in &pc.models {
                models.push(format!("{prov_name}/{}", m.id));
            }
        }
        if models.is_empty() || !models.iter().any(|m| m == &self.model) {
            models.push(self.model.clone());
        }
        models
    }
}

// ── Config File (lingclaw.json) ──────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct JsonConfig {
    settings: Option<JsonSettings>,
    models: Option<JsonModelsConfig>,
    agents: Option<JsonAgentsConfig>,
}

#[derive(Deserialize, Default)]
struct JsonSettings {
    port: Option<u16>,
    provider: Option<String>,
    #[serde(rename = "apiKey")]
    api_key: Option<String>,
    #[serde(rename = "apiBase")]
    api_base: Option<String>,
    #[serde(rename = "execTimeout")]
    exec_timeout: Option<u64>,
    #[serde(rename = "maxToolRounds")]
    max_tool_rounds: Option<usize>,
    #[serde(rename = "maxContextTokens")]
    max_context_tokens: Option<usize>,
    #[serde(rename = "maxOutputBytes")]
    max_output_bytes: Option<usize>,
    #[serde(rename = "maxFileBytes")]
    max_file_bytes: Option<usize>,
}

#[derive(Deserialize, Default)]
struct JsonModelsConfig {
    providers: Option<HashMap<String, JsonProviderConfig>>,
}

#[derive(Deserialize, Clone)]
struct JsonProviderConfig {
    #[serde(rename = "baseUrl")]
    base_url: String,
    #[serde(rename = "apiKey")]
    api_key: String,
    #[serde(default = "default_api_protocol")]
    api: String,
    #[serde(default)]
    models: Vec<JsonModelEntry>,
}

fn default_api_protocol() -> String {
    "openai-completions".to_string()
}

#[derive(Deserialize, Serialize, Clone, Default)]
struct JsonModelEntry {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "contextWindow")]
    context_window: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxTokens")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compat: Option<serde_json::Value>,
}

#[derive(Deserialize, Default)]
struct JsonAgentsConfig {
    defaults: Option<JsonAgentDefaults>,
}

#[derive(Deserialize, Default)]
struct JsonAgentDefaults {
    model: Option<JsonDefaultModel>,
}

#[derive(Deserialize, Default)]
struct JsonDefaultModel {
    primary: Option<String>,
}

fn config_dir_path() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    if home.is_empty() {
        return None;
    }
    Some(Path::new(&home).join(".lingclaw"))
}

fn config_file_path() -> Option<PathBuf> {
    Some(config_dir_path()?.join(".lingclaw.json"))
}

fn load_config_file() -> JsonConfig {
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

// ── First-Run Setup Wizard ───────────────────────────────────────────────────

fn prompt_line(msg: &str) -> String {
    print!("{msg}");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().lock().read_line(&mut buf).unwrap_or(0);
    buf.trim().to_string()
}

fn prompt_choice(options: &[&str]) -> usize {
    loop {
        for (i, opt) in options.iter().enumerate() {
            println!("  {}. {opt}", i + 1);
        }
        let input = prompt_line("> ");
        if let Ok(n) = input.parse::<usize>() {
            if n >= 1 && n <= options.len() {
                return n - 1;
            }
        }
        println!("Invalid choice. Please enter a number between 1 and {}.", options.len());
    }
}

/// Add the current binary's directory to the system PATH.
fn install_global_path() {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("   ❌ Cannot determine executable path: {e}");
            return;
        }
    };
    let dir = match exe.parent() {
        Some(d) => d.to_string_lossy().to_string(),
        None => {
            eprintln!("   ❌ Cannot determine executable directory");
            return;
        }
    };

    #[cfg(target_os = "windows")]
    {
        // Read current user PATH, append if not already present
        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command",
                   "[Environment]::GetEnvironmentVariable('Path','User')"])
            .output();
        match output {
            Ok(out) => {
                let current = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if current.to_lowercase().contains(&dir.to_lowercase()) {
                    println!("   ✅ Already in PATH");
                    return;
                }
                let new_path = if current.is_empty() {
                    dir.clone()
                } else {
                    format!("{current};{dir}")
                };
                let cmd = format!(
                    "[Environment]::SetEnvironmentVariable('Path','{}','User')",
                    new_path.replace('\'', "''")
                );
                let res = std::process::Command::new("powershell")
                    .args(["-NoProfile", "-Command", &cmd])
                    .status();
                match res {
                    Ok(s) if s.success() => {
                        println!("   ✅ Added to User PATH: {dir}");
                        // Also update the current process so child commands work immediately
                        if let Ok(machine) = std::env::var("Path") {
                            std::env::set_var("Path", format!("{new_path};{machine}"));
                        }
                    }
                    _ => eprintln!("   ❌ Failed to update PATH"),
                }
            }
            Err(e) => eprintln!("   ❌ Failed to read PATH: {e}"),
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        // Append to ~/.bashrc and ~/.zshrc if not already present
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            eprintln!("   ❌ Cannot determine HOME directory");
            return;
        }
        let export_line = format!("export PATH=\"{dir}:$PATH\"");
        let mut added = false;
        for rc in &[".bashrc", ".zshrc"] {
            let rc_path = Path::new(&home).join(rc);
            if !rc_path.exists() {
                continue;
            }
            let content = std::fs::read_to_string(&rc_path).unwrap_or_default();
            if content.contains(&dir) {
                continue;
            }
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&rc_path) {
                use std::io::Write;
                let _ = writeln!(f, "\n# LingClaw\n{export_line}");
                added = true;
            }
        }
        if added {
            println!("   ✅ Added to PATH in shell config. Run `source ~/.bashrc` or restart terminal.");
        } else {
            println!("   ✅ Already in PATH (or no .bashrc/.zshrc found)");
        }
    }
}

// ── CLI Subcommands ──────────────────────────────────────────────────────────

fn handle_cli_command(cmd: &str, port_override: Option<u16>) -> bool {
    match cmd {
        "start" => {
            let exe = std::env::current_exe().expect("cannot find executable");
            let mut extra_args: Vec<String> = vec!["--serve".to_string()];
            if let Some(p) = port_override {
                extra_args.push("--port".to_string());
                extra_args.push(p.to_string());
                println!("Starting LingClaw daemon on port {p}...");
            } else {
                println!("Starting LingClaw daemon...");
            }
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new(&exe)
                    .args(&extra_args)
                    .creation_flags(0x00000008) // DETACHED_PROCESS
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map(|c| println!("Started (PID {})", c.id()))
                    .map_err(|e| eprintln!("Failed to start: {e}"));
            }
            #[cfg(not(target_os = "windows"))]
            {
                let mut nohup_args: Vec<std::ffi::OsString> = vec![exe.into()];
                for a in &extra_args {
                    nohup_args.push(a.into());
                }
                let _ = std::process::Command::new("nohup")
                    .args(&nohup_args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map(|c| println!("Started (PID {})", c.id()))
                    .map_err(|e| eprintln!("Failed to start: {e}"));
            }
            true
        }
        "stop" => {
            let config = Config::load();
            let port = port_override.unwrap_or(config.port);
            println!("Stopping LingClaw on port {port}...");
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("powershell")
                    .args(["-NoProfile", "-Command", &format!(
                        "Get-NetTCPConnection -LocalPort {port} -ErrorAction SilentlyContinue | \
                         ForEach-Object {{ Stop-Process -Id $_.OwningProcess -Force }}"
                    )])
                    .status();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = std::process::Command::new("sh")
                    .args(["-c", &format!(
                        "lsof -ti:{port} | xargs -r kill"
                    )])
                    .status();
            }
            std::thread::sleep(Duration::from_millis(500));
            match std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
                Ok(_) => eprintln!("Warning: port {port} still in use"),
                Err(_) => println!("Stopped."),
            }
            true
        }
        "restart" => {
            handle_cli_command("stop", port_override);
            std::thread::sleep(Duration::from_secs(1));
            handle_cli_command("start", port_override);
            true
        }
        "health" => {
            let config = Config::load();
            let port = port_override.unwrap_or(config.port);
            let addr = format!("127.0.0.1:{port}");
            match std::net::TcpStream::connect_timeout(
                &addr.parse().expect("invalid addr"),
                Duration::from_secs(3),
            ) {
                Ok(mut stream) => {
                    use std::io::{Read, Write};
                    let req = format!(
                        "GET /api/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(req.as_bytes());
                    let mut buf = String::new();
                    let _ = stream.read_to_string(&mut buf);
                    // Extract JSON body after \r\n\r\n
                    if let Some(pos) = buf.find("\r\n\r\n") {
                        let body = buf[pos + 4..].trim();
                        println!("✅ {body}");
                    } else {
                        println!("✅ Running (port {port})");
                    }
                }
                Err(_) => eprintln!("❌ Not running (port {port} unreachable)"),
            }
            true
        }
        "update" => {
            let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            if !workspace.join("Cargo.toml").exists() {
                eprintln!("ERROR: Cargo.toml not found. Run `lingclaw update` from the source directory.");
                return true;
            }
            println!("Current version: v{VERSION}");
            println!("Pulling latest source...");
            let pull = std::process::Command::new("git")
                .args(["pull"])
                .status();
            match pull {
                Ok(s) if s.success() => println!("   ✅ git pull complete"),
                _ => {
                    eprintln!("   ❌ git pull failed");
                    return true;
                }
            }
            // Read version from updated Cargo.toml
            let new_version = std::fs::read_to_string(workspace.join("Cargo.toml"))
                .ok()
                .and_then(|content| {
                    content.lines().find_map(|line| {
                        let line = line.trim();
                        if line.starts_with("version") {
                            line.split('"').nth(1).map(|v| v.to_string())
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or_else(|| "unknown".to_string());
            if new_version == VERSION {
                println!("Already up to date (v{VERSION}).");
                return true;
            }
            println!("New version available: v{VERSION} → v{new_version}");
            println!("Building...");
            let build = std::process::Command::new("cargo")
                .args(["build", "--release"])
                .status();
            match build {
                Ok(s) if s.success() => {
                    println!("   ✅ Build complete (v{new_version})");
                    println!("Restarting...");
                    handle_cli_command("restart", port_override);
                }
                _ => eprintln!("   ❌ Build failed"),
            }
            true
        }
        "status" => {
            let config = Config::load();
            let port = port_override.unwrap_or(config.port);
            let addr = format!("127.0.0.1:{port}");

            // Check if running
            let running = std::net::TcpStream::connect_timeout(
                &addr.parse().expect("invalid addr"),
                Duration::from_secs(2),
            )
            .is_ok();

            println!("╔══════════════════════════════════════════════════════════╗");
            println!("║             🦀 LingClaw v{VERSION}                        ║");
            println!("╚══════════════════════════════════════════════════════════╝");
            println!();
            println!("  Version:       v{VERSION}");
            println!("  Service:       {}", if running { "✅ Running" } else { "❌ Stopped" });
            println!("  Address:       http://{addr}");
            println!("  Default model: {}", config.model);
            println!("  Provider:      {}", config.provider.label());
            println!("  API base:      {}", config.api_base);
            println!("  Exec timeout:  {}s", config.exec_timeout.as_secs());
            println!("  Max rounds:    {}", config.max_tool_rounds);
            println!("  Context limit: {} tokens", config.max_context_tokens);
            println!();

            if config.providers.is_empty() {
                println!("  Providers: (none configured)");
            } else {
                println!("  Providers:");
                println!();
                println!("  {:<16} {:<10} {:<30} {:>8}", "NAME", "API", "BASE URL", "MODELS");
                println!("  {}", "─".repeat(68));
                for (name, pc) in &config.providers {
                    println!("  {:<16} {:<10} {:<30} {:>8}",
                        name, pc.api,
                        if pc.base_url.len() > 30 { format!("{}…", &pc.base_url[..29]) } else { pc.base_url.clone() },
                        pc.models.len(),
                    );
                }
            }
            println!();

            // Collect all models across providers into a flat table
            struct ModelRow { name: String, id: String, provider: String, ctx: String, max_out: String, flags: String }
            let rows: Vec<ModelRow> = config.providers.iter().flat_map(|(pname, pc)| {
                pc.models.iter().map(move |m| ModelRow {
                    name: m.name.as_deref().unwrap_or(&m.id).to_string(),
                    id: m.id.clone(),
                    provider: pname.clone(),
                    ctx: m.context_window.map(|w| format!("{w}")).unwrap_or_else(|| "-".into()),
                    max_out: m.max_tokens.map(|t| format!("{t}")).unwrap_or_else(|| "-".into()),
                    flags: if m.reasoning.unwrap_or(false) { "reasoning".into() } else { String::new() },
                })
            }).collect();

            if rows.is_empty() {
                println!("  Models: (none configured)");
            } else {
                println!("  Models ({}):", rows.len());
                println!();
                println!("  {:<24} {:<30} {:<12} {:>8} {:>8}  FLAGS",
                    "NAME", "ID", "PROVIDER", "CTX", "MAX OUT");
                println!("  {}", "─".repeat(90));
                for r in &rows {
                    let dflt = if r.id == config.model { " *" } else { "" };
                    println!("  {:<24} {:<30} {:<12} {:>8} {:>8}  {}{}",
                        r.name, r.id, r.provider, r.ctx, r.max_out, r.flags, dflt);
                }
                println!();
                println!("  (* = default model)");
            }
            println!();

            // Check for newer version via git
            let _ = std::process::Command::new("git")
                .args(["fetch", "--quiet"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            if let Ok(output) = std::process::Command::new("git")
                .args(["show", "origin/main:Cargo.toml"])
                .output()
            {
                if output.status.success() {
                    let remote_cargo = String::from_utf8_lossy(&output.stdout);
                    let remote_ver = remote_cargo.lines().find_map(|line| {
                        let line = line.trim();
                        if line.starts_with("version") {
                            line.split('"').nth(1)
                        } else {
                            None
                        }
                    });
                    if let Some(rv) = remote_ver {
                        if rv != VERSION {
                            println!("  💡 New version available: v{VERSION} → v{rv}");
                            println!("     Run `lingclaw update` to upgrade.");
                            println!();
                        }
                    }
                }
            }

            true
        }
        "help" | "--help" | "-h" => {
            println!("🦀 LingClaw v{VERSION} — Personal AI Assistant");
            println!();
            println!("Usage: lingclaw <command> [options]");
            println!();
            println!("Commands:");
            println!("  start              Start the daemon");
            println!("  stop               Stop the daemon");
            println!("  restart            Restart the daemon");
            println!("  health             Health check (exit 0 = ok)");
            println!("  status             Show detailed service status");
            println!("  update             Check for updates, rebuild if newer");
            println!("  help               Show this help message");
            println!();
            println!("Options:");
            println!("  --port <PORT>      Override listening port");
            println!("  --install-daemon   Re-run Setup Wizard (backup existing config)");
            println!("  --version, -V      Show version");
            println!();
            println!("Without a command, runs the Setup Wizard on first launch,");
            println!("then starts the daemon in the background.");
            true
        }
        "--version" | "-V" => {
            println!("lingclaw v{VERSION}");
            true
        }
        _ => false,
    }
}

fn run_setup_wizard(force: bool) -> bool {
    let config_path = match config_file_path() {
        Some(p) => p,
        None => {
            eprintln!("Cannot determine home directory. Skipping setup wizard.");
            return false;
        }
    };

    if config_path.exists() {
        if !force {
            return true;
        }
        // Backup existing config before overwriting (never clobber previous backups)
        let mut bak_path = config_path.with_extension("json.bak");
        if bak_path.exists() {
            let mut n = 1u32;
            loop {
                let candidate = config_path.with_extension(format!("json.bak.{n}"));
                if !candidate.exists() {
                    bak_path = candidate;
                    break;
                }
                n += 1;
            }
        }
        if let Err(e) = std::fs::copy(&config_path, &bak_path) {
            eprintln!("WARNING: Failed to backup config to {}: {e}", bak_path.display());
        } else {
            eprintln!("Backed up existing config to {}", bak_path.display());
        }
    }

    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                 🦀 LingClaw Setup Wizard                  ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    // ── Step 1: Welcome ──────────────────────────────────────────────────
    println!("1. Hello, welcome to LingClaw. This might pose some security");
    println!("   issues, but it also offers you endless possibilities for");
    println!("   creation. Continue?");
    println!();
    let choice = prompt_choice(&["YES", "NO"]);
    if choice == 1 {
        println!("Bye!");
        return false;
    }
    println!();

    // ── Step 2: Model/Auth Provider ──────────────────────────────────────
    println!("2. Model/auth provider");
    println!();
    let provider_choice = prompt_choice(&["OpenAI", "Anthropic", "Skip for now"]);

    let mut providers: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let mut default_model: Option<String> = None;

    match provider_choice {
        0 => {
            // OpenAI
            println!();
            let base_url = prompt_line("  Base URL [https://api.openai.com/v1]: ");
            let base_url = if base_url.is_empty() {
                "https://api.openai.com/v1".to_string()
            } else {
                base_url
            };
            let api_key = prompt_line("  API Key: ");
            providers.insert(
                "openai".to_string(),
                json!({
                    "baseUrl": base_url,
                    "apiKey": api_key,
                    "api": "openai-completions",
                    "models": []
                }),
            );
            default_model = Some("openai/gpt-4o-mini".to_string());
        }
        1 => {
            // Anthropic
            println!();
            let base_url = prompt_line("  Base URL [https://api.anthropic.com]: ");
            let base_url = if base_url.is_empty() {
                "https://api.anthropic.com".to_string()
            } else {
                base_url
            };
            let api_key = prompt_line("  API Key: ");
            providers.insert(
                "anthropic".to_string(),
                json!({
                    "baseUrl": base_url,
                    "apiKey": api_key,
                    "api": "anthropic",
                    "models": []
                }),
            );
            default_model = Some("anthropic/claude-sonnet-4-20250514".to_string());
        }
        _ => {
            // Skip
        }
    }

    // ── Step 2b: Configure Models for Provider ───────────────────────────
    if !providers.is_empty() {
        println!();
        println!("   Configure models for your provider.");
        println!("   Enter model details (leave Name empty to finish):");
        let prov_name = providers.keys().next().unwrap().clone();
        let mut models_list: Vec<serde_json::Value> = Vec::new();
        loop {
            println!();
            let name = prompt_line("  Model Name (empty to finish): ");
            if name.is_empty() {
                break;
            }
            let id = prompt_line(&format!("  Model ID [{name}]: "));
            let id = if id.is_empty() { name.clone() } else { id };

            let reasoning_str = prompt_line("  Reasoning? (y/N): ").to_lowercase();
            let reasoning = reasoning_str == "y" || reasoning_str == "yes";

            let input_str = prompt_line("  Input types [text]: ");
            let input: Vec<String> = if input_str.is_empty() {
                vec!["text".to_string()]
            } else {
                input_str.split(',').map(|s| s.trim().to_string()).collect()
            };

            let ctx_str = prompt_line("  Context window tokens [128000]: ");
            let context_window: u64 = ctx_str.parse().unwrap_or(128000);

            let max_str = prompt_line("  Max output tokens [32768]: ");
            let max_tokens: u64 = max_str.parse().unwrap_or(32768);

            let thinking_fmt = prompt_line("  Thinking format (empty=none, e.g. qwen/openai): ");

            let mut model = json!({
                "id": id,
                "name": name,
                "reasoning": reasoning,
                "input": input,
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": context_window,
                "maxTokens": max_tokens,
            });
            if !thinking_fmt.is_empty() {
                model["compat"] = json!({ "thinkingFormat": thinking_fmt });
            }

            // Set first model as default if not already set
            if default_model.is_none() || models_list.is_empty() {
                default_model = Some(format!("{prov_name}/{id}"));
            }
            println!("   ✅ Added {name}");
            models_list.push(model);
        }

        // Inject models into the provider entry
        if let Some(prov) = providers.get_mut(&prov_name) {
            prov["models"] = json!(models_list);
        }
    }
    println!();

    // ── Step 3: Select Channel ───────────────────────────────────────────
    println!("3. Select channel (QuickStart)");
    println!();
    let _channel = prompt_choice(&["WebChat", "Skip for now"]);
    println!();

    // ── Step 4: Global PATH ────────────────────────────────────────────
    println!("4. Do you want to add LingClaw to the global PATH?");
    println!("   This enables CLI commands: lingclaw start/stop/restart/health/update");
    println!();
    let add_path = prompt_choice(&["YES", "NO"]);
    if add_path == 0 {
        install_global_path();
    }
    println!();

    // ── Step 5: Install ──────────────────────────────────────────────────
    println!("5. Start installation");
    prompt_line("   Press Enter to continue...");
    println!();

    // Build agents.defaults.models map from provider models
    let mut agent_models = serde_json::Map::new();
    for (prov_name, prov) in &providers {
        if let Some(models) = prov.get("models").and_then(|m| m.as_array()) {
            for m in models {
                if let Some(id) = m.get("id").and_then(|v| v.as_str()) {
                    agent_models.insert(format!("{prov_name}/{id}"), json!({}));
                }
            }
        }
    }

    // Build config JSON
    let mut config = json!({
        "settings": {
            "port": 3000,
            "execTimeout": 30,
            "maxToolRounds": 20,
            "maxContextTokens": 32000,
        },
        "models": {
            "providers": providers,
        },
        "agents": {
            "defaults": {
                "model": {
                    "primary": default_model.unwrap_or_else(|| "gpt-4o-mini".to_string()),
                },
                "models": agent_models,
            }
        }
    });

    // Add channel info if WebChat selected
    if _channel == 0 {
        config["channel"] = json!("webchat");
    }

    // Ensure ~/.lingclaw directory exists
    if let Some(dir) = config_dir_path() {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("ERROR: Failed to create config directory {}: {e}", dir.display());
            return false;
        }
    }

    // Write config file
    match serde_json::to_string_pretty(&config) {
        Ok(json_str) => {
            if let Err(e) = std::fs::write(&config_path, json_str) {
                eprintln!("ERROR: Failed to write config: {e}");
                return false;
            }
        }
        Err(e) => {
            eprintln!("ERROR: Failed to serialize config: {e}");
            return false;
        }
    }

    println!("   ✅ Configuration saved to {}", config_path.display());
    println!();
    true
}

// ══════════════════════════════════════════════════════════════════════════════
//  Data Models
// ══════════════════════════════════════════════════════════════════════════════

#[derive(Clone, Serialize, Deserialize, Debug)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: FunctionCall,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct FunctionCall {
    name: String,
    arguments: String,
}

// ══════════════════════════════════════════════════════════════════════════════
//  Session & AppState
// ══════════════════════════════════════════════════════════════════════════════

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn gen_session_id() -> String {
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}{:04x}", t.as_secs(), t.subsec_nanos() % 0xFFFF)
}

#[derive(Clone, Serialize, Deserialize)]
struct Session {
    id: String,
    name: String,
    messages: Vec<ChatMessage>,
    created_at: u64,
    updated_at: u64,
    tool_calls_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_override: Option<String>,
    #[serde(default = "default_think_level")]
    think_level: String,
    #[serde(skip)]
    workspace: PathBuf,
}

fn default_think_level() -> String {
    "off".to_string()
}

/// Per-session workspace: ~/.lingclaw/{sessionId}/workspace
fn session_workspace_path(session_id: &str) -> PathBuf {
    config_dir_path()
        .unwrap_or_else(|| PathBuf::from(".lingclaw"))
        .join(session_id)
        .join("workspace")
}

impl Session {
    fn new() -> Self {
        let id = gen_session_id();
        let workspace = session_workspace_path(&id);
        std::fs::create_dir_all(&workspace).ok();
        prompts::init_session_prompt_files(&workspace);
        Self {
            id,
            name: "New Chat".into(),
            messages: Vec::new(),
            created_at: now_epoch(),
            updated_at: now_epoch(),
            tool_calls_count: 0,
            model_override: None,
            think_level: "off".into(),
            workspace,
        }
    }

    fn effective_model<'a>(&'a self, default: &'a str) -> &'a str {
        self.model_override.as_deref().unwrap_or(default)
    }
}

struct AppState {
    config: Config,
    http: Client,
    sessions: Mutex<HashMap<String, Session>>,
}

// ══════════════════════════════════════════════════════════════════════════════
//  System Prompt
// ══════════════════════════════════════════════════════════════════════════════

fn build_system_prompt(config: &Config, workspace: &Path, model: &str) -> ChatMessage {
    let os_name = if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    let cwd = workspace.display();
    let tool_lines = tools::render_tool_prompt_lines(config);
    let persona = prompts::load_session_prompt_files(workspace);

    let prompt = format!(
        r#"{persona}

---

## Environment
- OS: {os_name}
- Working directory: {cwd}
- Model: {model}

## Available Tools
{tool_lines}"#,
        model = model,
        tool_lines = tool_lines,
        persona = persona,
    );

    ChatMessage {
        role: "system".into(),
        content: Some(prompt),
        tool_calls: None,
        tool_call_id: None,
    }
}

// ══════════════════════════════════════════════════════════════════════════════
//  Security
// ══════════════════════════════════════════════════════════════════════════════

const DANGEROUS_PATTERNS: &[&str] = &[
    "rm -rf /",
    "rm -rf /*",
    "mkfs.",
    "dd if=/dev",
    ":(){ :|:&",
    "> /dev/sda",
    "format c:",
    "del /f /s /q c:\\",
    "rd /s /q c:\\",
];

fn check_dangerous_command(cmd: &str) -> Option<&'static str> {
    let lower = cmd.to_lowercase();
    DANGEROUS_PATTERNS
        .iter()
        .find(|&&pattern| lower.contains(pattern))
        .copied()
}

fn resolve_path(path_str: &str, workspace: &Path) -> PathBuf {
    let p = Path::new(path_str);
    let candidate = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace.join(p)
    };
    // Normalize the path (resolve .., symlinks) and verify it stays inside workspace.
    // If canonicalize fails (file doesn't exist yet), manually strip `..` components.
    let resolved = candidate.canonicalize().unwrap_or_else(|_| {
        let mut parts = Vec::new();
        for comp in candidate.components() {
            match comp {
                std::path::Component::ParentDir => { parts.pop(); }
                std::path::Component::CurDir => {}
                c => parts.push(c.as_os_str().to_owned()),
            }
        }
        parts.iter().collect()
    });
    let ws_canonical = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    if resolved.starts_with(&ws_canonical) {
        resolved
    } else {
        // Path escapes workspace — clamp to workspace root
        eprintln!("SECURITY: path '{}' escapes workspace, clamped", path_str);
        ws_canonical
    }
}

// ══════════════════════════════════════════════════════════════════════════════
//  Utilities
// ══════════════════════════════════════════════════════════════════════════════

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!(
            "{}...\n[truncated at {} bytes, total {} bytes]",
            &s[..max],
            max,
            s.len()
        )
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn matches_glob(name: &str, pattern: &str) -> bool {
    if let Some(ext) = pattern.strip_prefix("*.") {
        name.ends_with(&format!(".{ext}"))
    } else if let Some(prefix) = pattern.strip_suffix('*') {
        name.starts_with(prefix)
    } else {
        name == pattern
    }
}

type WsTx = SplitSink<WebSocket, WsMsg>;

async fn ws_send(tx: &mut WsTx, data: &serde_json::Value) {
    let _ = tx.send(WsMsg::Text(data.to_string().into())).await;
}

// ══════════════════════════════════════════════════════════════════════════════
//  Tool Dispatch
// ══════════════════════════════════════════════════════════════════════════════

async fn execute_tool(name: &str, args_str: &str, config: &Config, http: &Client, workspace: &Path) -> String {
    tools::execute_tool(name, args_str, config, http, workspace).await
}

// ══════════════════════════════════════════════════════════════════════════════
//  Context Management
// ══════════════════════════════════════════════════════════════════════════════

fn estimate_tokens(messages: &[ChatMessage]) -> usize {
    messages
        .iter()
        .map(|m| {
            let content_len = m.content.as_ref().map(|c| c.len()).unwrap_or(0);
            let tc_len = m
                .tool_calls
                .as_ref()
                .map(|tcs| {
                    tcs.iter()
                        .map(|tc| tc.function.name.len() + tc.function.arguments.len())
                        .sum::<usize>()
                })
                .unwrap_or(0);
            (content_len + tc_len + 10) / 4 // rough ~4 chars per token
        })
        .sum()
}

fn prune_messages(messages: &mut Vec<ChatMessage>, max_tokens: usize) {
    // Keep: system message (index 0) + as many recent messages as fit.
    // Remove oldest non-system messages when over budget.
    while estimate_tokens(messages) > max_tokens && messages.len() > 2 {
        messages.remove(1);
    }
}

// ══════════════════════════════════════════════════════════════════════════════
//  Session Persistence
// ══════════════════════════════════════════════════════════════════════════════

fn sessions_dir() -> PathBuf {
    let dir = config_dir_path()
        .unwrap_or_else(|| PathBuf::from(".lingclaw"))
        .join("sessions");
    std::fs::create_dir_all(&dir).ok();
    dir
}

async fn save_session_to_disk(session: &Session) -> Result<(), String> {
    let path = sessions_dir().join(format!("{}.json", session.id));
    let data = serde_json::to_string_pretty(session).map_err(|e| e.to_string())?;
    tokio::fs::write(&path, data)
        .await
        .map_err(|e| e.to_string())
}

async fn load_session_from_disk(id: &str) -> Result<Session, String> {
    if id.contains('/') || id.contains('\\') || id.contains("..") {
        return Err("Invalid session ID".into());
    }
    let path = sessions_dir().join(format!("{id}.json"));
    let data = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| e.to_string())?;
    let mut session: Session = serde_json::from_str(&data).map_err(|e| e.to_string())?;
    session.workspace = session_workspace_path(&session.id);
    std::fs::create_dir_all(&session.workspace).ok();
    prompts::init_session_prompt_files(&session.workspace);
    Ok(session)
}

async fn list_saved_sessions() -> Vec<(String, String)> {
    let dir = sessions_dir();
    let mut result = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(id) = name.strip_suffix(".json") {
                if let Ok(content) = tokio::fs::read_to_string(entry.path()).await {
                    if let Ok(session) = serde_json::from_str::<Session>(&content) {
                        result.push((id.to_string(), session.name));
                    }
                }
            }
        }
    }
    result
}

// ══════════════════════════════════════════════════════════════════════════════
//  Chat Commands
// ══════════════════════════════════════════════════════════════════════════════

struct CommandResult {
    response: String,
    new_session_id: Option<String>,
    sessions_changed: bool,
}

async fn handle_command(
    input: &str,
    current_session_id: &str,
    state: &AppState,
) -> Option<CommandResult> {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd {
        "/new" => {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                let model = session.effective_model(&state.config.model).to_string();
                let system_msg = build_system_prompt(&state.config, &session.workspace, &model);
                session.messages = vec![system_msg];
                session.tool_calls_count = 0;
                session.updated_at = now_epoch();
                Some(CommandResult {
                    response: "Context cleared.".into(),
                    new_session_id: None,
                    sessions_changed: false,
                })
            } else {
                Some(CommandResult {
                    response: "Session not found".into(),
                    new_session_id: None,
                    sessions_changed: false,
                })
            }
        }

        "/sessions" => {
            let sessions = state.sessions.lock().await;
            if sessions.is_empty() {
                return Some(CommandResult {
                    response: "No active sessions.".into(),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let mut list: Vec<String> = sessions
                .iter()
                .map(|(id, s)| {
                    let marker = if id == current_session_id {
                        " ← active"
                    } else {
                        ""
                    };
                    format!(
                        "  {} | {} | msgs:{} tools:{}{}",
                        &id[..id.len().min(12)],
                        s.name,
                        s.messages.len(),
                        s.tool_calls_count,
                        marker,
                    )
                })
                .collect();
            list.sort();
            Some(CommandResult {
                response: format!("Sessions:\n{}", list.join("\n")),
                new_session_id: None,
                sessions_changed: false,
            })
        }

        "/switch" => {
            if arg.is_empty() {
                return Some(CommandResult {
                    response: "Usage: /switch <session_id>".into(),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let sessions = state.sessions.lock().await;
            // Allow prefix matching for convenience
            let matched: Vec<&String> = sessions
                .keys()
                .filter(|k| k.starts_with(arg))
                .collect();
            match matched.len() {
                0 => Some(CommandResult {
                    response: format!("No session matching '{arg}'"),
                    new_session_id: None,
                    sessions_changed: false,
                }),
                1 => {
                    let id = matched[0].clone();
                    let name = sessions.get(&id).map(|s| s.name.as_str()).unwrap_or("?");
                    Some(CommandResult {
                        response: format!("Switched to: {name} ({id})"),
                        new_session_id: Some(id),
                        sessions_changed: true,
                    })
                }
                _ => Some(CommandResult {
                    response: format!("Ambiguous: {} sessions match '{arg}'", matched.len()),
                    new_session_id: None,
                    sessions_changed: false,
                }),
            }
        }

        "/rename" => {
            if arg.is_empty() {
                return Some(CommandResult {
                    response: "Usage: /rename <new_name>".into(),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                session.name = arg.to_string();
                Some(CommandResult {
                    response: format!("Renamed to: {arg}"),
                    new_session_id: None,
                    sessions_changed: true,
                })
            } else {
                Some(CommandResult {
                    response: "Current session not found".into(),
                    new_session_id: None,
                    sessions_changed: false,
                })
            }
        }

        "/save" => {
            let sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get(current_session_id) {
                match save_session_to_disk(session).await {
                    Ok(()) => Some(CommandResult {
                        response: format!(
                            "Session saved: {} ({})",
                            session.name, current_session_id
                        ),
                        new_session_id: None,
                        sessions_changed: false,
                    }),
                    Err(e) => Some(CommandResult {
                        response: format!("Save error: {e}"),
                        new_session_id: None,
                        sessions_changed: false,
                    }),
                }
            } else {
                Some(CommandResult {
                    response: "Current session not found".into(),
                    new_session_id: None,
                    sessions_changed: false,
                })
            }
        }

        "/load" => {
            if arg.is_empty() {
                let saved = list_saved_sessions().await;
                if saved.is_empty() {
                    return Some(CommandResult {
                        response: "No saved sessions on disk.".into(),
                        new_session_id: None,
                        sessions_changed: false,
                    });
                }
                let list: Vec<String> = saved
                    .iter()
                    .map(|(id, name)| format!("  {id} → {name}"))
                    .collect();
                return Some(CommandResult {
                    response: format!(
                        "Saved sessions:\n{}\n\nUse /load <id> to restore.",
                        list.join("\n")
                    ),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            match load_session_from_disk(arg).await {
                Ok(session) => {
                    let id = session.id.clone();
                    let name = session.name.clone();
                    let mut sessions = state.sessions.lock().await;
                    sessions.insert(id.clone(), session);
                    Some(CommandResult {
                        response: format!("Loaded: {name} ({id})"),
                        new_session_id: Some(id),
                        sessions_changed: true,
                    })
                }
                Err(e) => Some(CommandResult {
                    response: format!("Load error: {e}"),
                    new_session_id: None,
                    sessions_changed: false,
                }),
            }
        }

        "/delete" => {
            if arg.is_empty() {
                return Some(CommandResult {
                    response: "Usage: /delete <session_id>".into(),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            if arg == current_session_id || current_session_id.starts_with(arg) {
                return Some(CommandResult {
                    response: "Cannot delete the active session. /switch first.".into(),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let mut sessions = state.sessions.lock().await;
            let to_remove: Vec<String> = sessions
                .keys()
                .filter(|k| k.starts_with(arg))
                .cloned()
                .collect();
            match to_remove.len() {
                0 => Some(CommandResult {
                    response: format!("No session matching '{arg}'"),
                    new_session_id: None,
                    sessions_changed: false,
                }),
                1 => {
                    let id = to_remove[0].clone();
                    sessions.remove(&id);
                    // Clean up persisted session file
                    let session_file = sessions_dir().join(format!("{id}.json"));
                    if session_file.exists() {
                        let _ = std::fs::remove_file(&session_file);
                    }
                    // Clean up session workspace directory
                    let ws_dir = session_workspace_path(&id);
                    if ws_dir.exists() {
                        let _ = std::fs::remove_dir_all(&ws_dir);
                    }
                    Some(CommandResult {
                        response: format!("Deleted session {id} (memory + disk)"),
                        new_session_id: None,
                        sessions_changed: true,
                    })
                }
                _ => Some(CommandResult {
                    response: format!("Ambiguous: {} sessions match", to_remove.len()),
                    new_session_id: None,
                    sessions_changed: false,
                }),
            }
        }

        "/model" => {
            if arg.is_empty() {
                let sessions = state.sessions.lock().await;
                let model = sessions
                    .get(current_session_id)
                    .map(|s| s.effective_model(&state.config.model))
                    .unwrap_or(&state.config.model);
                let available = state.config.available_models();
                let list = available
                    .iter()
                    .map(|m| {
                        if m == model {
                            format!("  * {m} (current)")
                        } else {
                            format!("    {m}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Some(CommandResult {
                    response: format!("Available models:\n{list}\n\nUse /model <name> to switch."),
                    new_session_id: None,
                    sessions_changed: false,
                })
            } else {
                let mut sessions = state.sessions.lock().await;
                if let Some(session) = sessions.get_mut(current_session_id) {
                    session.model_override = Some(arg.to_string());
                    Some(CommandResult {
                        response: format!("Model switched to: {arg}"),
                        new_session_id: None,
                        sessions_changed: true,
                    })
                } else {
                    Some(CommandResult {
                        response: "Session not found".into(),
                        new_session_id: None,
                        sessions_changed: false,
                    })
                }
            }
        }

        "/status" => {
            let sessions = state.sessions.lock().await;
            match sessions.get(current_session_id) {
                Some(s) => {
                    let model_ref = s.effective_model(&state.config.model);
                    let tokens = estimate_tokens(&s.messages);
                    Some(CommandResult {
                        response: format!(
                            "agent: LingClaw\n\
                             model: {model_ref}\n\
                             context: {tokens}/{max_ctx}\n\
                             think: {think}",
                            max_ctx = state.config.max_context_tokens,
                            think = s.think_level,
                        ),
                        new_session_id: None,
                        sessions_changed: false,
                    })
                }
                None => Some(CommandResult {
                    response: "No active session".into(),
                    new_session_id: None,
                    sessions_changed: false,
                }),
            }
        }

        "/clear" => {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                let model = session.effective_model(&state.config.model).to_string();
                let system_msg = build_system_prompt(&state.config, &session.workspace, &model);
                session.messages = vec![system_msg];
                session.tool_calls_count = 0;
                session.updated_at = now_epoch();
                Some(CommandResult {
                    response: "Session cleared. System prompt preserved.".into(),
                    new_session_id: None,
                    sessions_changed: false,
                })
            } else {
                Some(CommandResult {
                    response: "Session not found".into(),
                    new_session_id: None,
                    sessions_changed: false,
                })
            }
        }

        "/skills" => {
            let list = tools::tool_specs()
                .iter()
                .map(|spec| {
                    let short = spec.description.split('.').next().unwrap_or(spec.description);
                    format!("  {} → {}", spec.name, short)
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(CommandResult {
                response: format!("Skills:\n{list}"),
                new_session_id: None,
                sessions_changed: false,
            })
        }

        "/think" => {
            const VALID_LEVELS: &[&str] = &["off", "minimal", "low", "medium", "high", "xhigh"];
            if arg.is_empty() {
                let sessions = state.sessions.lock().await;
                let level = sessions
                    .get(current_session_id)
                    .map(|s| s.think_level.as_str())
                    .unwrap_or("off");
                return Some(CommandResult {
                    response: format!("think: {level}\nUsage: /think <off|minimal|low|medium|high|xhigh>"),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let level = arg.to_lowercase();
            if !VALID_LEVELS.contains(&level.as_str()) {
                return Some(CommandResult {
                    response: format!("Invalid think level: {arg}\nValid: off, minimal, low, medium, high, xhigh"),
                    new_session_id: None,
                    sessions_changed: false,
                });
            }
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                session.think_level = level.clone();
                Some(CommandResult {
                    response: format!("Think mode set to: {level}"),
                    new_session_id: None,
                    sessions_changed: true,
                })
            } else {
                Some(CommandResult {
                    response: "Session not found".into(),
                    new_session_id: None,
                    sessions_changed: false,
                })
            }
        }

        "/help" => Some(CommandResult {
            response: "\
Commands:
  /new             Clear context
  /status          Show session status
  /model [name]    Show or switch model
  /think [level]   Set thinking mode (off|minimal|low|medium|high|xhigh)
  /skills          List available skills
  /sessions        List all sessions
  /switch <id>     Switch to session (prefix match)
  /rename <name>   Rename current session
  /save            Save current session to disk
  /load [id]       List or load saved sessions
  /delete <id>     Delete a session
  /clear           Clear messages (keep system prompt)
  /help            Show this help"
                .into(),
            new_session_id: None,
            sessions_changed: false,
        }),

        _ => None,
    }
}

// ══════════════════════════════════════════════════════════════════════════════
//  WebSocket Handler
// ══════════════════════════════════════════════════════════════════════════════

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut tx, mut rx) = socket.split();

    // Create a session for this connection
    let mut session = Session::new();
    let model = session.effective_model(&state.config.model).to_string();
    let system_msg = build_system_prompt(&state.config, &session.workspace, &model);
    session.messages.push(system_msg);
    let mut current_session_id = session.id.clone();

    ws_send(
        &mut tx,
        &json!({"type":"session","id":&current_session_id,"name":"New Chat"}),
    )
    .await;

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(current_session_id.clone(), session);
    }

    while let Some(Ok(msg)) = rx.next().await {
        let text = match msg {
            WsMsg::Text(t) => t.to_string(),
            WsMsg::Close(_) => break,
            _ => continue,
        };

        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }

        // ── Slash commands ──────────────────────────────────────────────
        if trimmed.starts_with('/') {
            if let Some(result) = handle_command(trimmed, &current_session_id, &state).await {
                ws_send(
                    &mut tx,
                    &json!({"type":"system","content":result.response}),
                )
                .await;

                if let Some(new_id) = result.new_session_id {
                    current_session_id = new_id.clone();
                    ws_send(&mut tx, &json!({"type":"session_switched","id":new_id})).await;
                }
                if result.sessions_changed {
                    send_sessions_list(&mut tx, &state, &current_session_id).await;
                }
            } else {
                ws_send(
                    &mut tx,
                    &json!({"type":"system","content":"Unknown command. Type /help."}),
                )
                .await;
            }
            continue;
        }

        // ── Normal message → Agent loop ─────────────────────────────────
        {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(&current_session_id) {
                session.messages.push(ChatMessage {
                    role: "user".into(),
                    content: Some(text),
                    tool_calls: None,
                    tool_call_id: None,
                });
                session.updated_at = now_epoch();
            }
        }

        let mut completed = false;
        for round in 0..state.config.max_tool_rounds {
            // Snapshot messages (prune if needed)
            let (msgs_snapshot, model, workspace) = {
                let mut sessions = state.sessions.lock().await;
                let session = match sessions.get_mut(&current_session_id) {
                    Some(s) => s,
                    None => break,
                };
                // Refresh system prompt so prompt-file edits take effect mid-session
                let model_str = session.effective_model(&state.config.model).to_string();
                let fresh_system = build_system_prompt(&state.config, &session.workspace, &model_str);
                if let Some(first) = session.messages.first_mut() {
                    if first.role == "system" {
                        *first = fresh_system;
                    }
                }
                prune_messages(&mut session.messages, state.config.max_context_tokens);
                (
                    session.messages.clone(),
                    model_str,
                    session.workspace.clone(),
                )
            };

            ws_send(&mut tx, &json!({"type":"start","round":round + 1})).await;

            let resolved = state.config.resolve_model(&model);
            match providers::call_llm_stream(
                &state.http,
                &resolved,
                &msgs_snapshot,
                &mut tx,
            )
            .await
            {
                Ok(resp) => {
                    let has_tools = resp.message.tool_calls.is_some();

                    // Push assistant message to session
                    {
                        let mut sessions = state.sessions.lock().await;
                        if let Some(session) = sessions.get_mut(&current_session_id) {
                            session.messages.push(resp.message.clone());
                            session.updated_at = now_epoch();
                        }
                    }

                    if let Some(tool_calls) = &resp.message.tool_calls {
                        for tc in tool_calls {
                            ws_send(
                                &mut tx,
                                &json!({
                                    "type":"tool_call",
                                    "id": tc.id,
                                    "name": tc.function.name,
                                    "arguments": tc.function.arguments,
                                }),
                            )
                            .await;

                            let result = execute_tool(
                                &tc.function.name,
                                &tc.function.arguments,
                                &state.config,
                                &state.http,
                                &workspace,
                            )
                            .await;

                            ws_send(
                                &mut tx,
                                &json!({
                                    "type":"tool_result",
                                    "id": tc.id,
                                    "name": tc.function.name,
                                    "result": result,
                                }),
                            )
                            .await;

                            // Push tool result to session
                            let mut sessions = state.sessions.lock().await;
                            if let Some(session) = sessions.get_mut(&current_session_id) {
                                session.messages.push(ChatMessage {
                                    role: "tool".into(),
                                    content: Some(result),
                                    tool_calls: None,
                                    tool_call_id: Some(tc.id.clone()),
                                });
                                session.tool_calls_count += 1;
                            }
                        }
                        // Loop continues → LLM processes tool results
                    }

                    if !has_tools {
                        ws_send(&mut tx, &json!({"type":"done"})).await;
                        completed = true;
                        break;
                    }
                }
                Err(e) => {
                    ws_send(&mut tx, &json!({"type":"error","content":e})).await;
                    completed = true;
                    break;
                }
            }
        }

        if !completed {
            ws_send(
                &mut tx,
                &json!({
                    "type":"system",
                    "content": format!(
                        "Reached maximum tool rounds ({}). Stopping.",
                        state.config.max_tool_rounds
                    )
                }),
            )
            .await;
        }
    }

    // ── Cleanup: auto-save and remove ephemeral session on disconnect ────
    {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get(&current_session_id) {
            // Auto-save non-trivial sessions (more than just the system prompt)
            if session.messages.len() > 1 {
                let _ = save_session_to_disk(session).await;
            }
        }
        sessions.remove(&current_session_id);
    }
}

async fn send_sessions_list(tx: &mut WsTx, state: &AppState, active_id: &str) {
    let sessions = state.sessions.lock().await;
    let list: Vec<serde_json::Value> = sessions
        .iter()
        .map(|(id, s)| {
            json!({
                "id": id,
                "name": s.name,
                "messages": s.messages.len(),
                "active": id == active_id,
            })
        })
        .collect();
    ws_send(tx, &json!({"type":"sessions_list","sessions":list})).await;
}

// ══════════════════════════════════════════════════════════════════════════════
//  HTTP API
// ══════════════════════════════════════════════════════════════════════════════

async fn api_health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sessions = state.sessions.lock().await;
    Json(json!({
        "status": "ok",
        "model": state.config.model,
        "sessions": sessions.len(),
    }))
}

async fn api_sessions(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let sessions = state.sessions.lock().await;
    let list: Vec<serde_json::Value> = sessions
        .iter()
        .map(|(id, s)| {
            json!({
                "id": id,
                "name": s.name,
                "messages": s.messages.len(),
                "tool_calls": s.tool_calls_count,
                "model": s.effective_model(&state.config.model),
                "created_at": s.created_at,
                "updated_at": s.updated_at,
            })
        })
        .collect();
    Json(json!({"sessions": list}))
}

// ══════════════════════════════════════════════════════════════════════════════
//  Main
// ══════════════════════════════════════════════════════════════════════════════

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Parse --port <N> from anywhere in args
    let port_override: Option<u16> = args.windows(2)
        .find(|w| w[0] == "--port")
        .and_then(|w| w[1].parse().ok());

    // --version / -V: print and exit early
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("lingclaw v{VERSION}");
        return;
    }

    // CLI subcommands: lingclaw start|stop|restart|health|status|update
    if args.len() > 1
        && !args[1].starts_with('-')
        && handle_cli_command(&args[1], port_override)
    {
        return;
    }

    let force_wizard = args.iter().any(|a| a == "--install-daemon");
    let serve_mode = args.iter().any(|a| a == "--serve");

    // First-run setup wizard (before loading config)
    if !run_setup_wizard(force_wizard) {
        return;
    }

    // Default behavior (no --serve): start as daemon
    if !serve_mode {
        handle_cli_command("start", port_override);
        return;
    }

    let config = Config::load();
    let port = port_override.unwrap_or(config.port);

    if config.api_key.is_empty() && config.providers.is_empty() {
        eprintln!(
            "WARNING: {} is not set and no config file providers found. LLM calls will fail.",
            match config.provider {
                Provider::Anthropic => "ANTHROPIC_API_KEY",
                Provider::OpenAI => "OPENAI_API_KEY",
            }
        );
    }

    eprintln!("Config:");
    eprintln!("  Provider:      {}", config.provider.label());
    eprintln!("  Model:         {}", config.model);
    eprintln!("  API base:      {}", config.api_base);
    if !config.providers.is_empty() {
        let names: Vec<&str> = config.providers.keys().map(|s| s.as_str()).collect();
        let total: usize = config.providers.values().map(|p| p.models.len()).sum();
        eprintln!("  Config providers: {} ({} models)", names.join(", "), total);
    }
    eprintln!("  Exec timeout:  {}s", config.exec_timeout.as_secs());
    eprintln!("  Max rounds:    {}", config.max_tool_rounds);
    eprintln!("  Context limit: {} tokens", config.max_context_tokens);

    let state = Arc::new(AppState {
        config,
        http: Client::new(),
        sessions: Mutex::new(HashMap::new()),
    });

    let app = Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/health", get(api_health))
        .route("/api/sessions", get(api_sessions))
        .fallback_service(ServeDir::new("static").append_index_html_on_directories(true))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    println!("🦀 LingClaw v2 listening on http://{addr}");
    println!("   Tools: think, exec, read_file, write_file, patch_file, list_dir, search_files, http_fetch");

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");
    axum::serve(listener, app).await.expect("server failed");
}
