#[cfg(test)]
use std::future::Future;
use std::io::{self, BufRead, Write};
#[allow(unused_imports)]
use std::net::SocketAddr;
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::json;

use crate::{Config, DEFAULT_PORT, VERSION, config_dir_path, config_file_path, prompts};

static MCP_PREFLIGHT_ID: AtomicU64 = AtomicU64::new(1);
const MCP_PREFLIGHT_TIMEOUT_SECS: u64 = 5;

// ── Interactive Helpers ──────────────────────────────────────────────────────

fn prompt_line(msg: &str) -> String {
    print!("{msg}");
    io::stdout().flush().ok();
    let mut buf = String::new();
    io::stdin().lock().read_line(&mut buf).unwrap_or(0);
    buf.trim().to_string()
}

fn prompt_secret(msg: &str) -> String {
    print!("{msg}");
    io::stdout().flush().ok();
    match rpassword::read_password() {
        Ok(value) => value.trim().to_string(),
        Err(_) => {
            eprintln!("\n  Warning: secure input unavailable, input will be visible.");
            prompt_line("  ")
        }
    }
}

fn prompt_choice(options: &[&str]) -> usize {
    loop {
        for (i, opt) in options.iter().enumerate() {
            println!("  {}. {opt}", i + 1);
        }
        let input = prompt_line("> ");
        if let Ok(n) = input.parse::<usize>()
            && n >= 1
            && n <= options.len()
        {
            return n - 1;
        }
        println!(
            "Invalid choice. Please enter a number between 1 and {}.",
            options.len()
        );
    }
}

fn inspect_mcp_preflight(
    config: &Config,
) -> Result<Vec<crate::tools::mcp::McpServerLoadReport>, String> {
    inspect_mcp_reports(preflight_config(config), Some(MCP_PREFLIGHT_TIMEOUT_SECS))
}

fn inspect_mcp_check(
    config: &Config,
) -> Result<Vec<crate::tools::mcp::McpServerLoadReport>, String> {
    inspect_mcp_reports(config.clone(), None)
}

fn inspect_mcp_reports(
    config: Config,
    total_timeout_secs: Option<u64>,
) -> Result<Vec<crate::tools::mcp::McpServerLoadReport>, String> {
    let enabled_count = config
        .mcp_servers
        .values()
        .filter(|server| server.enabled)
        .count();
    if enabled_count == 0 {
        return Ok(Vec::new());
    }

    let workspace = std::env::temp_dir().join(format!(
        "lingclaw-mcp-preflight-{}-{}",
        std::process::id(),
        MCP_PREFLIGHT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&workspace)
        .map_err(|error| format!("failed to create MCP preflight workspace: {error}"))?;

    let thread_workspace = workspace.clone();
    let thread = std::thread::Builder::new()
        .name("lingclaw-mcp-preflight".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| format!("failed to build MCP preflight runtime: {error}"))?;
            Ok::<_, String>(runtime.block_on(run_mcp_inspection(
                &config,
                &thread_workspace,
                total_timeout_secs,
            )))
        })
        .map_err(|error| format!("failed to spawn MCP preflight worker: {error}"))?;

    let result = match thread.join() {
        Ok(result) => result,
        Err(_) => Err("MCP preflight worker panicked".to_string()),
    };
    std::fs::remove_dir_all(&workspace).ok();
    result?
}

fn preflight_config(config: &Config) -> Config {
    let mut preflight = config.clone();
    let timeout = Duration::from_secs(MCP_PREFLIGHT_TIMEOUT_SECS);
    preflight.tool_timeout = timeout;
    for server in preflight.mcp_servers.values_mut() {
        server.timeout_secs = Some(
            server
                .timeout_secs
                .unwrap_or(MCP_PREFLIGHT_TIMEOUT_SECS)
                .min(MCP_PREFLIGHT_TIMEOUT_SECS),
        );
    }
    preflight
}

#[cfg(test)]
async fn with_preflight_timeout<F, T>(future: F) -> Result<T, String>
where
    F: Future<Output = T>,
{
    tokio::time::timeout(Duration::from_secs(MCP_PREFLIGHT_TIMEOUT_SECS), future)
        .await
        .map_err(|_| {
            format!(
                "MCP preflight timed out after {}s",
                MCP_PREFLIGHT_TIMEOUT_SECS
            )
        })
}

async fn run_mcp_inspection(
    config: &Config,
    workspace: &Path,
    total_timeout_secs: Option<u64>,
) -> Result<Vec<crate::tools::mcp::McpServerLoadReport>, String> {
    let inspection = crate::tools::mcp::inspect_servers(config, workspace);
    match total_timeout_secs {
        Some(timeout_secs) => tokio::time::timeout(Duration::from_secs(timeout_secs), inspection)
            .await
            .map_err(|_| format!("MCP preflight timed out after {timeout_secs}s")),
        None => Ok(inspection.await),
    }
}

fn print_mcp_preflight(config: &Config) {
    let enabled_count = config
        .mcp_servers
        .values()
        .filter(|server| server.enabled)
        .count();
    if enabled_count == 0 {
        return;
    }

    println!(
        "MCP preflight: checking {enabled_count} server(s), total timeout capped at {}s...",
        MCP_PREFLIGHT_TIMEOUT_SECS
    );

    let reports = match inspect_mcp_preflight(config) {
        Ok(reports) => reports,
        Err(error) => {
            eprintln!("  ⚠ MCP preflight failed: {error} — service startup will continue");
            return;
        }
    };

    for report in reports {
        if let Some(error) = report.error {
            eprintln!(
                "  ⚠ {}: failed to load ({error}) — service startup will continue",
                report.server_name
            );
            continue;
        }

        let summary = if report.tool_names.is_empty() {
            "0 tools".to_string()
        } else {
            format!(
                "{} tools: {}",
                report.tool_names.len(),
                report.tool_names.join(", ")
            )
        };
        println!("  ✅ {}: loaded {summary}", report.server_name);
    }
}

fn loopback_addr(port: u16) -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], port))
}

fn current_git_branch() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

fn remote_cargo_toml_refs() -> Vec<String> {
    let mut refs = Vec::new();
    if let Some(branch) = current_git_branch() {
        refs.push(format!("origin/{branch}:Cargo.toml"));
    }
    refs.push("origin/main:Cargo.toml".to_string());
    refs.push("origin/master:Cargo.toml".to_string());
    refs.dedup();
    refs
}

fn read_remote_version() -> Option<String> {
    for git_ref in remote_cargo_toml_refs() {
        let output = std::process::Command::new("git")
            .args(["show", &git_ref])
            .output()
            .ok()?;
        if !output.status.success() {
            continue;
        }

        let remote_cargo = String::from_utf8_lossy(&output.stdout);
        if let Some(version) = remote_cargo.lines().find_map(|line| {
            let line = line.trim();
            if line.starts_with("version") {
                line.split('"').nth(1).map(|value| value.to_string())
            } else {
                None
            }
        }) {
            return Some(version);
        }
    }

    None
}

fn mcp_check_succeeded(reports: &[crate::tools::mcp::McpServerLoadReport]) -> bool {
    reports.iter().all(|report| report.error.is_none())
}

fn print_mcp_check(config: &Config) -> bool {
    let enabled_count = config
        .mcp_servers
        .values()
        .filter(|server| server.enabled)
        .count();
    if enabled_count == 0 {
        println!("No enabled MCP servers configured.");
        return true;
    }

    println!("MCP check: inspecting {enabled_count} server(s) with configured runtime timeouts...");
    let reports = match inspect_mcp_check(config) {
        Ok(reports) => reports,
        Err(error) => {
            eprintln!("  ❌ MCP check failed: {error}");
            return false;
        }
    };
    let success = mcp_check_succeeded(&reports);

    for report in reports {
        if let Some(error) = report.error {
            eprintln!("  ❌ {}: {error}", report.server_name);
            continue;
        }

        let summary = if report.tool_names.is_empty() {
            "0 tools".to_string()
        } else {
            format!(
                "{} tools: {}",
                report.tool_names.len(),
                report.tool_names.join(", ")
            )
        };
        println!("  ✅ {}: {summary}", report.server_name);
    }

    success
}

#[cfg(not(target_os = "windows"))]
const SYSTEMD_SERVICE_NAME: &str = "lingclaw.service";

#[cfg(not(target_os = "windows"))]
const SYSTEMD_SERVICE_PATH: &str = "/etc/systemd/system/lingclaw.service";

#[cfg(not(target_os = "windows"))]
fn shell_profile_paths(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".profile"),
        home.join(".bashrc"),
        home.join(".zshrc"),
    ]
}

#[cfg(not(target_os = "windows"))]
fn append_export_once(rc_path: &Path, dir: &str, export_line: &str) -> io::Result<bool> {
    let content = std::fs::read_to_string(rc_path).unwrap_or_default();
    if content.contains(export_line) || content.contains(dir) {
        return Ok(false);
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(rc_path)?;
    if !content.is_empty() && !content.ends_with('\n') {
        writeln!(file)?;
    }
    writeln!(file, "# LingClaw")?;
    writeln!(file, "{export_line}")?;
    Ok(true)
}

#[cfg(not(target_os = "windows"))]
fn systemd_service_path() -> PathBuf {
    PathBuf::from(SYSTEMD_SERVICE_PATH)
}

#[cfg(not(target_os = "windows"))]
fn systemd_available() -> bool {
    std::process::Command::new("systemctl")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn systemd_service_installed() -> bool {
    systemd_available() && systemd_service_path().exists()
}

#[cfg(not(target_os = "windows"))]
fn systemd_query(args: &[&str]) -> bool {
    std::process::Command::new("systemctl")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn systemd_service_active() -> bool {
    systemd_query(&["is-active", "--quiet", SYSTEMD_SERVICE_NAME])
}

#[cfg(not(target_os = "windows"))]
fn systemd_service_enabled() -> bool {
    systemd_query(&["is-enabled", "--quiet", SYSTEMD_SERVICE_NAME])
}

#[cfg(not(target_os = "windows"))]
fn running_as_root() -> bool {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "0")
        .unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn privileged_command(program: &str) -> std::process::Command {
    if running_as_root() {
        std::process::Command::new(program)
    } else {
        let mut command = std::process::Command::new("sudo");
        command.arg(program);
        command
    }
}

#[cfg(not(target_os = "windows"))]
fn run_systemctl(args: &[&str]) -> io::Result<bool> {
    let mut command = privileged_command("systemctl");
    let status = command.args(args).status()?;
    Ok(status.success())
}

#[cfg(not(target_os = "windows"))]
fn passwd_home_dir(user: &str) -> Option<String> {
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        let mut parts = line.split(':');
        let name = parts.next()?;
        if name != user {
            return None;
        }
        let _password = parts.next()?;
        let _uid = parts.next()?;
        let _gid = parts.next()?;
        let _gecos = parts.next()?;
        let home = parts.next()?;
        if home.is_empty() {
            None
        } else {
            Some(home.to_string())
        }
    })
}

#[cfg(not(target_os = "windows"))]
fn resolve_home_for_user(user: &str) -> String {
    if std::env::var("SUDO_USER").ok().as_deref() == Some(user) {
        return passwd_home_dir(user).unwrap_or_else(|| format!("/home/{user}"));
    }

    if std::env::var("USER").ok().as_deref() == Some(user) {
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                return home;
            }
        }
    }

    passwd_home_dir(user).unwrap_or_else(|| {
        if user == "root" {
            "/root".to_string()
        } else {
            format!("/home/{user}")
        }
    })
}

#[cfg(not(target_os = "windows"))]
fn sanitize_systemd_value(value: &str) -> String {
    value.replace(|c: char| c.is_control(), "_")
}

#[cfg(not(target_os = "windows"))]
fn quote_systemd_value(value: &str) -> String {
    let sanitized = sanitize_systemd_value(value);
    format!(
        "\"{}\"",
        sanitized.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

#[cfg(not(target_os = "windows"))]
fn build_systemd_service_unit(exe: &Path, working_dir: &Path, user: &str, home: &str) -> String {
    let user = sanitize_systemd_value(user);
    let working_dir = quote_systemd_value(&working_dir.display().to_string());
    let home = quote_systemd_value(&format!("HOME={}", sanitize_systemd_value(home)));
    let exec_start = format!(
        "{} --serve",
        quote_systemd_value(&exe.display().to_string())
    );
    format!(
        "[Unit]\nDescription=LingClaw AI Assistant\nAfter=network.target\n\n[Service]\nType=simple\nUser={user}\nWorkingDirectory={}\nEnvironment={}\nExecStart={}\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=multi-user.target\n",
        working_dir, home, exec_start
    )
}

#[cfg(not(target_os = "windows"))]
fn install_systemd_service() -> bool {
    if !systemd_available() {
        eprintln!("❌ systemctl not available on this system.");
        return true;
    }

    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("❌ Cannot determine executable path: {e}");
            return true;
        }
    };
    let working_dir = exe
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    let user = std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "root".to_string());
    let home = resolve_home_for_user(&user);
    let service_body = build_systemd_service_unit(&exe, &working_dir, &user, &home);

    let temp_dir = config_dir_path().unwrap_or_else(|| PathBuf::from("."));
    if let Err(e) = std::fs::create_dir_all(&temp_dir) {
        eprintln!(
            "❌ Failed to prepare temp directory {}: {e}",
            temp_dir.display()
        );
        return true;
    }
    let temp_service = temp_dir.join(SYSTEMD_SERVICE_NAME);
    if let Err(e) = std::fs::write(&temp_service, service_body) {
        eprintln!(
            "❌ Failed to write service file {}: {e}",
            temp_service.display()
        );
        return true;
    }

    let temp_service_str = temp_service.to_string_lossy().to_string();
    let service_path_str = systemd_service_path().to_string_lossy().to_string();
    let install_status = privileged_command("install")
        .args(["-m", "0644", &temp_service_str, &service_path_str])
        .status();
    match install_status {
        Ok(status) if status.success() => {}
        Ok(_) => {
            eprintln!(
                "❌ Failed to install systemd unit into {}",
                service_path_str
            );
            return true;
        }
        Err(e) => {
            eprintln!("❌ Failed to run install for systemd unit: {e}");
            return true;
        }
    }

    if !run_systemctl(&["daemon-reload"]).unwrap_or(false) {
        eprintln!("❌ Failed to reload systemd daemon.");
        return true;
    }
    if !run_systemctl(&["enable", "--now", SYSTEMD_SERVICE_NAME]).unwrap_or(false) {
        eprintln!("❌ Failed to enable/start {}.", SYSTEMD_SERVICE_NAME);
        return true;
    }

    println!("✅ systemd service installed: {}", service_path_str);
    println!("   Service: {SYSTEMD_SERVICE_NAME}");
    println!("   Logs: journalctl -u {SYSTEMD_SERVICE_NAME} -f");
    true
}

fn print_start_details(port: u16, manager: &str) {
    println!("  Manager: {manager}");
    println!("  Address: http://127.0.0.1:{port}");
    if let Some(path) = config_file_path() {
        println!("  Config:  {}", path.display());
    }
}

fn build_s3_start_detail_lines(
    config: &Config,
    raw_s3_present: bool,
    settings_enable_s3: Option<bool>,
    env_enable_s3: Option<bool>,
) -> Vec<String> {
    if let Some(s3) = config.s3.as_ref() {
        let mut lines = vec![
            "  S3:      enabled".to_string(),
            format!("  S3 URL:  {}", s3.endpoint),
            format!(
                "  S3 To:   bucket={} region={} prefix={}",
                s3.bucket, s3.region, s3.prefix
            ),
            format!(
                "  S3 Opts: presign={}s lifecycle={}d",
                s3.url_expiry_secs, s3.lifecycle_days
            ),
        ];

        if env_enable_s3 == Some(true) && settings_enable_s3 == Some(false) {
            lines.push(
                "  S3 Note: LINGCLAW_ENABLE_S3=true overrides settings.enableS3=false".to_string(),
            );
        } else if env_enable_s3 == Some(true) {
            lines.push("  S3 Note: enabled by LINGCLAW_ENABLE_S3=true".to_string());
        }

        return lines;
    }

    if env_enable_s3 == Some(false) {
        let mut lines = vec!["  S3:      disabled by LINGCLAW_ENABLE_S3=false".to_string()];
        if raw_s3_present {
            lines.push("  S3 Note: s3 section exists but runtime uploads are disabled".to_string());
        }
        return lines;
    }

    if settings_enable_s3 == Some(false) {
        let mut lines = vec!["  S3:      disabled by settings.enableS3=false".to_string()];
        if raw_s3_present {
            lines.push("  S3 Note: s3 section exists but runtime uploads are disabled".to_string());
        }
        return lines;
    }

    if raw_s3_present {
        return vec![
            "  S3:      configured but incomplete (missing required s3 fields)".to_string(),
        ];
    }

    vec!["  S3:      not configured".to_string()]
}

fn print_s3_start_details(config: &Config) {
    let raw_cfg = crate::config::load_config_file();
    let settings_enable_s3 = raw_cfg
        .settings
        .as_ref()
        .and_then(|settings| settings.enable_s3);
    let env_enable_s3 = crate::config::parse_boolish_env("LINGCLAW_ENABLE_S3");
    let raw_s3_present = raw_cfg.s3.is_some();

    for line in
        build_s3_start_detail_lines(config, raw_s3_present, settings_enable_s3, env_enable_s3)
    {
        println!("{line}");
    }
}

fn print_start_details_with_s3(port: u16, manager: &str, config: &Config) {
    print_start_details(port, manager);
    print_s3_start_details(config);
}

// ── PATH Installation ────────────────────────────────────────────────────────

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
            .args([
                "-NoProfile",
                "-Command",
                "[Environment]::GetEnvironmentVariable('Path','User')",
            ])
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
                    }
                    _ => eprintln!("   ❌ Failed to update PATH"),
                }
            }
            Err(e) => eprintln!("   ❌ Failed to read PATH: {e}"),
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            eprintln!("   ❌ Cannot determine HOME directory");
            return;
        }
        let home_path = PathBuf::from(&home);
        let export_line = format!("export PATH=\"{dir}:$PATH\"");
        let mut added = false;
        for rc_path in shell_profile_paths(&home_path) {
            match append_export_once(&rc_path, &dir, &export_line) {
                Ok(changed) => {
                    added |= changed;
                }
                Err(e) => {
                    eprintln!("   ❌ Failed to update {}: {e}", rc_path.display());
                }
            }
        }
        if added {
            println!("   ✅ Added to PATH for current shell and future login shells.");
            println!("   Updated: ~/.profile, ~/.bashrc, ~/.zshrc (when available)");
        } else {
            println!("   ✅ Already in PATH");
        }
    }
}

/// On Windows, rename the target exe to `.old` so `cargo build` can produce a fresh one.
/// Returns the `.old` path if a rename was performed, for cleanup after build.
fn rename_target_exe_for_build(source_dir: &std::path::Path) -> Option<PathBuf> {
    #[cfg(not(windows))]
    {
        let _ = source_dir;
        return None;
    }
    #[cfg(windows)]
    {
        let exe_name = "lingclaw.exe";
        let target_exe = source_dir.join("target").join("release").join(exe_name);
        if target_exe.exists() {
            let old_exe = target_exe.with_extension("exe.old");
            // Remove stale .old if present
            let _ = std::fs::remove_file(&old_exe);
            if std::fs::rename(&target_exe, &old_exe).is_ok() {
                return Some(old_exe);
            }
        }
        None
    }
}

fn copy_dir_recursive(source: &Path, target: &Path) -> io::Result<()> {
    if !source.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = target_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&source_path, &target_path)?;
        }
    }
    Ok(())
}

fn install_frontend_assets(source_dir: &Path, install_dir: &Path) -> io::Result<()> {
    let source_static = source_dir.join("static");
    if !source_static.is_dir() {
        return Ok(());
    }

    let target_static = install_dir.join("static");
    let same_dir = source_static
        .canonicalize()
        .ok()
        .zip(target_static.canonicalize().ok())
        .is_some_and(|(lhs, rhs)| lhs == rhs);
    if same_dir {
        return Ok(());
    }

    copy_dir_recursive(&source_static, &target_static)
}

/// Copy system skills from source `docs/reference/skills/` to `~/.lingclaw/system-skills/`.
fn install_system_skills(source_dir: &Path) -> io::Result<()> {
    let source_skills = source_dir.join("docs").join("reference").join("skills");
    if !source_skills.is_dir() {
        return Ok(());
    }
    let target = match config_dir_path() {
        Some(d) => d.join("system-skills"),
        None => return Ok(()),
    };
    // Remove stale target so we get a clean copy
    if target.is_dir() {
        std::fs::remove_dir_all(&target)?;
    }
    copy_dir_recursive(&source_skills, &target)
}

/// Copy system agents from source `docs/reference/agents/` to `~/.lingclaw/system-agents/`.
fn install_system_agents(source_dir: &Path) -> io::Result<()> {
    let source_agents = source_dir.join("docs").join("reference").join("agents");
    if !source_agents.is_dir() {
        return Ok(());
    }
    let target = match config_dir_path() {
        Some(d) => d.join("system-agents"),
        None => return Ok(()),
    };
    // Remove stale target so we get a clean copy
    if target.is_dir() {
        std::fs::remove_dir_all(&target)?;
    }
    copy_dir_recursive(&source_agents, &target)
}

fn install_built_binary(built_exe: &Path, current_exe: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::fs::copy(built_exe, current_exe)?;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let install_dir = current_exe.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot determine install directory",
            )
        })?;
        let file_name = current_exe.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot determine executable name",
            )
        })?;
        let temp_exe = install_dir.join(format!(
            ".{}.tmp-{}",
            file_name.to_string_lossy(),
            std::process::id()
        ));

        let _ = std::fs::remove_file(&temp_exe);
        std::fs::copy(built_exe, &temp_exe)?;

        if let Err(e) = std::fs::rename(&temp_exe, current_exe) {
            let _ = std::fs::remove_file(&temp_exe);
            return Err(e);
        }

        Ok(())
    }
}

pub(crate) fn is_default_model_row(config: &Config, provider: &str, model_id: &str) -> bool {
    let full_ref = format!("{provider}/{model_id}");
    let default_model = config.resolved_model_ref(&config.model);
    full_ref == default_model || (config.providers.is_empty() && model_id == config.model)
}

// ── CLI Subcommands ──────────────────────────────────────────────────────────

fn handle_start_command(port_override: Option<u16>) -> bool {
    let config = Config::load();
    print_mcp_preflight(&config);
    #[cfg(not(target_os = "windows"))]
    let managed_by_systemd = systemd_service_installed();
    #[cfg(target_os = "windows")]
    let managed_by_systemd = false;
    let effective_port = if managed_by_systemd {
        if let Some(port) = port_override
            && port != config.port
        {
            eprintln!(
                "Warning: --port is ignored when LingClaw is managed by systemd. Update config and restart the service instead."
            );
        }
        config.port
    } else {
        port_override.unwrap_or(config.port)
    };

    #[cfg(not(target_os = "windows"))]
    if managed_by_systemd {
        println!("Starting LingClaw via systemd...");
        print_start_details_with_s3(effective_port, "systemd", &config);
        match run_systemctl(&["start", SYSTEMD_SERVICE_NAME]) {
            Ok(true) => println!("Started {}.", SYSTEMD_SERVICE_NAME),
            Ok(false) => eprintln!("Failed to start {}.", SYSTEMD_SERVICE_NAME),
            Err(e) => eprintln!("Failed to start {}: {e}", SYSTEMD_SERVICE_NAME),
        }
        return true;
    }

    let exe = match std::env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("Failed to resolve current executable: {error}");
            return true;
        }
    };
    let mut extra_args: Vec<String> = vec!["--serve".to_string()];
    if let Some(p) = port_override {
        extra_args.push("--port".to_string());
        extra_args.push(p.to_string());
    }
    println!("Starting LingClaw daemon...");
    print_start_details_with_s3(
        effective_port,
        if cfg!(target_os = "windows") {
            "detached-process"
        } else {
            "nohup"
        },
        &config,
    );
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let mut command = std::process::Command::new(&exe);
        command
            .args(&extra_args)
            .creation_flags(0x00000008)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(parent) = exe.parent() {
            command.current_dir(parent);
        }
        let _ = command
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
        let mut command = std::process::Command::new("nohup");
        command
            .args(&nohup_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Some(parent) = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(PathBuf::from))
        {
            command.current_dir(parent);
        }
        let _ = command
            .spawn()
            .map(|c| println!("Started (PID {})", c.id()))
            .map_err(|e| eprintln!("Failed to start: {e}"));
    }
    true
}

fn handle_stop_command(port_override: Option<u16>) -> bool {
    let config = Config::load();
    let port = port_override.unwrap_or(config.port);
    #[cfg(not(target_os = "windows"))]
    if systemd_service_installed() {
        println!("Stopping LingClaw systemd service...");
        match run_systemctl(&["stop", SYSTEMD_SERVICE_NAME]) {
            Ok(true) => println!("Stopped {}.", SYSTEMD_SERVICE_NAME),
            Ok(false) => eprintln!("Failed to stop {}.", SYSTEMD_SERVICE_NAME),
            Err(e) => eprintln!("Failed to stop {}: {e}", SYSTEMD_SERVICE_NAME),
        }
        return true;
    }
    let loopback = SocketAddr::from(([127, 0, 0, 1], port));
    println!("Stopping LingClaw on port {port}...");

    let graceful = std::net::TcpStream::connect_timeout(&loopback, Duration::from_secs(2)).is_ok();

    if graceful {
        let token = config_dir_path()
            .map(|d| d.join(format!("shutdown-{port}.token")))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();

        let shutdown_ok = std::process::Command::new(if cfg!(windows) { "powershell" } else { "sh" })
            .args(if cfg!(windows) {
                vec![
                    "-NoProfile".to_string(),
                    "-Command".to_string(),
                    format!(
                        "try {{ Invoke-RestMethod -Method Post -Uri http://127.0.0.1:{port}/api/shutdown -Headers @{{Authorization='Bearer {token}'}} -TimeoutSec 5 | Out-Null; $true }} catch {{ $false }}"
                    ),
                ]
            } else {
                vec![
                    "-c".to_string(),
                    format!("curl -sf -X POST http://127.0.0.1:{port}/api/shutdown -H 'Authorization: Bearer {token}' -o /dev/null 2>/dev/null"),
                ]
            })
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if shutdown_ok {
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(500));
                if std::net::TcpStream::connect_timeout(&loopback, Duration::from_millis(200))
                    .is_err()
                {
                    println!("Stopped (graceful).");
                    return true;
                }
            }
            eprintln!("Graceful shutdown timed out, force-killing...");
        }
    }

    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "Get-NetTCPConnection -LocalPort {port} -ErrorAction SilentlyContinue | \
                 Select-Object -ExpandProperty OwningProcess -Unique | \
                 ForEach-Object {{ Stop-Process -Id $_ -Force -ErrorAction SilentlyContinue }}"
                ),
            ])
            .status();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = std::process::Command::new("sh")
            .args(["-c", &format!("lsof -ti:{port} | xargs -r kill -9")])
            .status();
    }
    std::thread::sleep(Duration::from_millis(500));
    match std::net::TcpStream::connect(format!("127.0.0.1:{port}")) {
        Ok(_) => eprintln!("Warning: port {port} still in use"),
        Err(_) => println!("Stopped."),
    }
    true
}

fn handle_restart_command(port_override: Option<u16>) -> bool {
    let config = Config::load();
    print_mcp_preflight(&config);
    #[cfg(not(target_os = "windows"))]
    if systemd_service_installed() {
        println!("Restarting LingClaw via systemd...");
        print_start_details_with_s3(config.port, "systemd", &config);
        match run_systemctl(&["restart", SYSTEMD_SERVICE_NAME]) {
            Ok(true) => println!("Restarted {}.", SYSTEMD_SERVICE_NAME),
            Ok(false) => eprintln!("Failed to restart {}.", SYSTEMD_SERVICE_NAME),
            Err(e) => eprintln!("Failed to restart {}: {e}", SYSTEMD_SERVICE_NAME),
        }
        return true;
    }
    handle_stop_command(port_override);
    std::thread::sleep(Duration::from_secs(1));
    handle_start_command(port_override)
}

fn handle_mcp_check_command() -> bool {
    let config = Config::load();
    if !print_mcp_check(&config) {
        process::exit(1);
    }
    true
}

fn handle_health_command(port_override: Option<u16>) -> bool {
    let config = Config::load();
    let port = port_override.unwrap_or(config.port);
    let addr = format!("127.0.0.1:{port}");
    match std::net::TcpStream::connect_timeout(&loopback_addr(port), Duration::from_secs(3)) {
        Ok(mut stream) => {
            use std::io::{Read, Write};
            let req =
                format!("GET /api/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
            let _ = stream.write_all(req.as_bytes());
            let mut buf = String::new();
            let _ = stream.read_to_string(&mut buf);
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

fn handle_update_command(port_override: Option<u16>) -> bool {
    let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if !workspace.join("Cargo.toml").exists() {
        eprintln!("ERROR: Cargo.toml not found. Run `lingclaw update` from the source directory.");
        return true;
    }
    println!("Current version: v{VERSION}");
    println!("Pulling latest source...");
    let pull = std::process::Command::new("git").args(["pull"]).status();
    match pull {
        Ok(s) if s.success() => println!("   ✅ git pull complete"),
        _ => {
            eprintln!("   ❌ git pull failed");
            return true;
        }
    }

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

    let config = Config::load();
    let check_port = port_override.unwrap_or(config.port);
    let was_running =
        std::net::TcpStream::connect_timeout(&loopback_addr(check_port), Duration::from_secs(2))
            .is_ok();
    if was_running {
        println!("Stopping service before build...");
        handle_stop_command(port_override);
        let exe = std::env::current_exe().ok();
        let mut released = false;
        for i in 0..10 {
            if let Some(ref path) = exe {
                if std::fs::OpenOptions::new().write(true).open(path).is_ok() {
                    released = true;
                    break;
                }
            } else {
                std::thread::sleep(Duration::from_secs(2));
                released = true;
                break;
            }
            if i < 9 {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        if !released {
            eprintln!(
                "   ❌ Failed to release binary file lock after 5s. Is the process still running?"
            );
            return true;
        }
    }

    println!("Building...");
    let old_exe = rename_target_exe_for_build(&std::env::current_dir().unwrap_or_default());
    let build = std::process::Command::new("cargo")
        .args(["build", "--release"])
        .status();
    match build {
        Ok(s) if s.success() => {
            if let Some(ref p) = old_exe {
                let _ = std::fs::remove_file(p);
            }
            println!("   ✅ Build complete (v{new_version})");
            match install_system_skills(&workspace) {
                Ok(()) => println!("   ✅ System skills updated"),
                Err(e) => eprintln!("   ⚠ Failed to update system skills: {e}"),
            }
            match install_system_agents(&workspace) {
                Ok(()) => println!("   ✅ System agents updated"),
                Err(e) => eprintln!("   ⚠ Failed to update system agents: {e}"),
            }
            println!("Starting...");
            handle_start_command(port_override);
        }
        _ => {
            if let Some(ref p) = old_exe {
                let target = p.with_extension("exe");
                let _ = std::fs::rename(p, &target);
            }
            eprintln!("   ❌ Build failed");
            if was_running {
                println!("Restarting previous version...");
                handle_start_command(port_override);
            }
        }
    }
    true
}

fn handle_status_command(port_override: Option<u16>) -> bool {
    let config = Config::load();
    let port = port_override.unwrap_or(config.port);
    let addr = format!("127.0.0.1:{port}");
    #[cfg(not(target_os = "windows"))]
    let manager = if systemd_service_installed() {
        "systemd"
    } else {
        "nohup"
    };
    #[cfg(target_os = "windows")]
    let manager = "detached-process";

    let running =
        std::net::TcpStream::connect_timeout(&loopback_addr(port), Duration::from_secs(2)).is_ok();

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║             🦀 LingClaw v{VERSION}                        ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();
    println!("  Version:       v{VERSION}");
    println!(
        "  Service:       {}",
        if running {
            "✅ Running"
        } else {
            "❌ Stopped"
        }
    );
    println!("  Manager:       {}", manager);
    println!("  Address:       http://{addr}");
    #[cfg(not(target_os = "windows"))]
    if systemd_service_installed() {
        println!(
            "  systemd:       {} / {}",
            if systemd_service_enabled() {
                "enabled"
            } else {
                "disabled"
            },
            if systemd_service_active() {
                "active"
            } else {
                "inactive"
            }
        );
        println!("  Service file:  {}", SYSTEMD_SERVICE_PATH);
    }
    println!("  Default model: {}", config.model);
    println!("  Provider:      {}", config.provider.label());
    println!("  API base:      {}", config.api_base);
    println!("  Exec timeout:  {}s", config.exec_timeout.as_secs());
    println!("  Tool timeout:  {}s", config.tool_timeout.as_secs());
    println!(
        "  Agent timeout: {}",
        crate::config::format_sub_agent_timeout(config.sub_agent_timeout)
    );
    println!("  LLM retries:  {}", config.max_llm_retries);
    println!("  Context limit: {} tokens", config.max_context_tokens);
    print_s3_start_details(&config);
    println!();

    if config.providers.is_empty() {
        println!("  Providers: (none configured)");
    } else {
        println!("  Providers:");
        println!();
        println!(
            "  {:<16} {:<10} {:<30} {:>8}",
            "NAME", "API", "BASE URL", "MODELS"
        );
        println!("  {}", "─".repeat(68));
        for (name, pc) in &config.providers {
            println!(
                "  {:<16} {:<10} {:<30} {:>8}",
                name,
                pc.api,
                if pc.base_url.len() > 30 {
                    format!("{}…", &pc.base_url[..29])
                } else {
                    pc.base_url.clone()
                },
                pc.models.len(),
            );
        }
    }
    println!();

    struct ModelRow {
        name: String,
        id: String,
        provider: String,
        ctx: String,
        max_out: String,
        flags: String,
    }
    let rows: Vec<ModelRow> = config
        .providers
        .iter()
        .flat_map(|(pname, pc)| {
            pc.models.iter().map(move |m| ModelRow {
                name: m.name.as_deref().unwrap_or(&m.id).to_string(),
                id: m.id.clone(),
                provider: pname.clone(),
                ctx: m
                    .context_window
                    .map(|w| format!("{w}"))
                    .unwrap_or_else(|| "-".into()),
                max_out: m
                    .max_tokens
                    .map(|t| format!("{t}"))
                    .unwrap_or_else(|| "-".into()),
                flags: if m.reasoning.unwrap_or(false) {
                    "reasoning".into()
                } else {
                    String::new()
                },
            })
        })
        .collect();

    if rows.is_empty() {
        println!("  Models: (none configured)");
    } else {
        println!("  Models ({}):", rows.len());
        println!();
        println!(
            "  {:<24} {:<30} {:<12} {:>8} {:>8}  FLAGS",
            "NAME", "ID", "PROVIDER", "CTX", "MAX OUT"
        );
        println!("  {}", "─".repeat(90));
        for r in &rows {
            let dflt = if is_default_model_row(&config, &r.provider, &r.id) {
                " *"
            } else {
                ""
            };
            println!(
                "  {:<24} {:<30} {:<12} {:>8} {:>8}  {}{}",
                r.name, r.id, r.provider, r.ctx, r.max_out, r.flags, dflt
            );
        }
        println!();
        println!("  (* = default model)");
    }
    println!();

    // ── MCP Servers ──
    if config.mcp_servers.is_empty() {
        println!("  MCP Servers: (none configured)");
    } else {
        println!("  MCP Servers ({}):", config.mcp_servers.len());
        println!();
        println!(
            "  {:<20} {:<8} {:<30} {:>10}",
            "NAME", "STATUS", "COMMAND", "TIMEOUT"
        );
        println!("  {}", "─".repeat(72));
        for (name, srv) in &config.mcp_servers {
            let status = if srv.enabled { "enabled" } else { "disabled" };
            let cmd_display = if srv.args.is_empty() {
                srv.command.clone()
            } else {
                format!("{} {}", srv.command, srv.args.join(" "))
            };
            let cmd_display = if cmd_display.chars().count() > 30 {
                let s: String = cmd_display.chars().take(29).collect();
                format!("{s}…")
            } else {
                cmd_display
            };
            let timeout = srv
                .timeout_secs
                .map(|t| format!("{t}s"))
                .unwrap_or_else(|| format!("{}s", config.tool_timeout.as_secs()));
            println!(
                "  {:<20} {:<8} {:<30} {:>10}",
                name, status, cmd_display, timeout
            );
        }
    }
    println!();

    // ── Skills ──
    // Use a dummy workspace path — we only care about system + global layers here.
    let dummy_workspace = std::path::Path::new("");
    let system_skills =
        prompts::discover_skills_by_source(dummy_workspace, prompts::SkillSource::System);
    let global_skills =
        prompts::discover_skills_by_source(dummy_workspace, prompts::SkillSource::Global);

    let total = system_skills.len() + global_skills.len();
    if total == 0 {
        println!("  Skills: (none discovered)");
    } else {
        println!(
            "  Skills ({} system, {} global):",
            system_skills.len(),
            global_skills.len()
        );
        println!();
        println!("  {:<24} {:<8} DESCRIPTION", "NAME", "SOURCE");
        println!("  {}", "─".repeat(72));
        for skill in system_skills.iter().chain(global_skills.iter()) {
            let desc = if skill.description.chars().count() > 38 {
                let s: String = skill.description.chars().take(37).collect();
                format!("{s}…")
            } else if skill.description.is_empty() {
                "-".into()
            } else {
                skill.description.clone()
            };
            println!("  {:<24} {:<8} {}", skill.name, skill.source.label(), desc);
        }
    }
    println!();

    let _ = std::process::Command::new("git")
        .args(["fetch", "--quiet"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if let Some(remote_ver) = read_remote_version()
        && remote_ver != VERSION
    {
        println!("  💡 New version available: v{VERSION} → v{remote_ver}");
        println!("     Run `lingclaw update` to upgrade.");
        println!();
    }

    true
}

// ── Doctor ───────────────────────────────────────────────────────────────────

/// Minimum Rust version required for edition 2024.
const MIN_RUSTC_VERSION: (u32, u32, u32) = (1, 85, 0);

fn parse_version_triple(s: &str) -> Option<(u32, u32, u32)> {
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

fn detect_rustc_version() -> Option<String> {
    let output = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // "rustc 1.85.0 (..." → "1.85.0"
    text.split_whitespace().nth(1).map(|v| v.to_string())
}

fn detect_cargo_version() -> Option<String> {
    let output = std::process::Command::new("cargo")
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace().nth(1).map(|v| v.to_string())
}

fn detect_git_version() -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // "git version 2.45.0" → "2.45.0"  (also handles "git version 2.45.0.windows.1")
    text.split_whitespace().nth(2).map(|v| v.to_string())
}

#[cfg(not(target_os = "windows"))]
fn command_exists(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn read_local_source_version() -> Option<String> {
    let cargo_toml = std::env::current_dir().ok()?.join("Cargo.toml");
    let content = std::fs::read_to_string(cargo_toml).ok()?;
    if !content.contains("name = \"lingclaw\"") {
        return None;
    }
    content.lines().find_map(|line| {
        let line = line.trim();
        if line.starts_with("version") {
            line.split('"').nth(1).map(|v| v.to_string())
        } else {
            None
        }
    })
}

fn handle_doctor_command() -> bool {
    println!("🩺 LingClaw Doctor — Install Readiness Check");
    println!();

    let mut all_ok = true;

    // ── 1. Rust toolchain ────────────────────────────────────────────────
    print!("  Rust toolchain ............. ");
    let rustc_ok = if let Some(ver_str) = detect_rustc_version() {
        if let Some(triple) = parse_version_triple(&ver_str) {
            if triple >= MIN_RUSTC_VERSION {
                println!("✅ rustc {ver_str}");
                true
            } else {
                println!(
                    "❌ rustc {ver_str} (need >= {}.{}.{})",
                    MIN_RUSTC_VERSION.0, MIN_RUSTC_VERSION.1, MIN_RUSTC_VERSION.2
                );
                false
            }
        } else {
            println!("⚠ rustc {ver_str} (cannot parse version)");
            false
        }
    } else {
        println!("❌ not found");
        false
    };
    if !rustc_ok {
        all_ok = false;
    }

    // ── 2. Cargo ─────────────────────────────────────────────────────────
    print!("  Cargo ...................... ");
    let cargo_ok = if let Some(ver_str) = detect_cargo_version() {
        println!("✅ cargo {ver_str}");
        true
    } else {
        println!("❌ not found");
        false
    };
    if !cargo_ok {
        all_ok = false;
    }

    // ── 3. Git ───────────────────────────────────────────────────────────
    print!("  Git ........................ ");
    let git_ok = if let Some(ver_str) = detect_git_version() {
        println!("✅ git {ver_str}");
        true
    } else {
        println!("❌ not found (needed for update/remote checks)");
        false
    };
    if !git_ok {
        all_ok = false;
    }

    // ── 4. Platform tooling ──────────────────────────────────────────────
    #[cfg(target_os = "windows")]
    {
        print!("  PowerShell ................. ");
        let ps_ok = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", "exit 0"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ps_ok {
            println!("✅ available");
        } else {
            println!("⚠ not found (needed for PATH registration)");
        }
    }

    #[cfg(not(target_os = "windows"))]
    let has_systemd;
    #[cfg(not(target_os = "windows"))]
    {
        print!("  systemd .................... ");
        has_systemd = command_exists("systemctl");
        if has_systemd {
            println!("✅ available");
        } else {
            println!("⚠ not found (lingclaw will use nohup instead)");
        }

        print!("  nohup ...................... ");
        if command_exists("nohup") {
            println!("✅ available");
        } else if has_systemd {
            println!("⚠ not found (systemd is available, so this is fine)");
        } else {
            println!("❌ neither systemd nor nohup found for background execution");
            all_ok = false;
        }
    }

    // ── 5. Source vs installed ────────────────────────────────────────────
    print!("  Source vs installed ......... ");
    let (source_ok, source_ver) = if let Some(src_ver) = read_local_source_version() {
        let src_parts: Vec<u32> = src_ver.split('.').filter_map(|s| s.parse().ok()).collect();
        let cur_parts: Vec<u32> = VERSION.split('.').filter_map(|s| s.parse().ok()).collect();
        match src_parts.cmp(&cur_parts) {
            std::cmp::Ordering::Greater => {
                println!("💡 v{} → v{src_ver} (upgrade available)", VERSION);
                (true, Some(src_ver))
            }
            std::cmp::Ordering::Equal => {
                println!("✅ v{VERSION} (same)");
                (true, Some(src_ver))
            }
            std::cmp::Ordering::Less => {
                println!("❌ source v{src_ver} < installed v{VERSION}");
                all_ok = false;
                (false, Some(src_ver))
            }
        }
    } else {
        println!("⚠ not in a LingClaw source directory");
        (true, None) // not a blocker when run outside source tree
    };

    // ── 6. Source vs remote ──────────────────────────────────────────────
    print!("  Source vs remote ........... ");
    let remote_ok = {
        // Try git fetch first so remote refs are fresh.
        let _ = std::process::Command::new("git")
            .args(["fetch", "--quiet"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if let Some(remote_ver) = read_remote_version() {
            if let Some(ref src_ver) = source_ver {
                let src_parts: Vec<u32> =
                    src_ver.split('.').filter_map(|s| s.parse().ok()).collect();
                let remote_parts: Vec<u32> = remote_ver
                    .split('.')
                    .filter_map(|s| s.parse().ok())
                    .collect();
                match src_parts.cmp(&remote_parts) {
                    std::cmp::Ordering::Equal => {
                        println!("✅ v{src_ver} (up to date)");
                        true
                    }
                    std::cmp::Ordering::Greater => {
                        println!("✅ v{src_ver} (ahead of remote v{remote_ver})");
                        true
                    }
                    std::cmp::Ordering::Less => {
                        println!("💡 v{src_ver} → v{remote_ver} (behind remote)");
                        all_ok = false;
                        false
                    }
                }
            } else {
                println!("⚠ skipped (no local source)");
                true
            }
        } else {
            println!("⚠ cannot read remote (no git remote?)");
            true // not fatal
        }
    };

    println!();

    if all_ok {
        println!("✅ All checks passed. Ready to install.");
        if source_ver.is_some() {
            println!("   Run `lingclaw install` to proceed.");
        }
        return true;
    }

    // ── Offer to fix ─────────────────────────────────────────────────────
    println!("Some checks need attention:");
    println!();
    let mut fixes: Vec<(&str, &str)> = Vec::new();
    if !rustc_ok {
        println!(
            "  • Rust toolchain needs >= {}.{}.{}",
            MIN_RUSTC_VERSION.0, MIN_RUSTC_VERSION.1, MIN_RUSTC_VERSION.2
        );
        fixes.push(("rust", "rustup update stable"));
    }
    if !cargo_ok && rustc_ok {
        println!("  • Cargo not found — usually installed with rustup");
        fixes.push(("cargo", "rustup update stable"));
    }
    if !git_ok {
        println!("  • Git not found — install git from https://git-scm.com");
        // Cannot auto-fix git installation portably.
    }
    #[cfg(not(target_os = "windows"))]
    if !has_systemd && !command_exists("nohup") {
        println!("  • No background execution tool (systemd or nohup)");
        println!("    → Install nohup (coreutils) or enable systemd.");
    }
    if !remote_ok {
        println!("  • Local source is behind remote");
        fixes.push(("source", "git pull"));
    }
    if !source_ok {
        println!("  • Source version is older than installed binary");
        println!("    → Pull the latest source or check out a newer branch.");
    }
    println!();

    if fixes.is_empty() {
        return true;
    }

    // Deduplicate (rust + cargo may both suggest `rustup update stable`)
    fixes.dedup_by_key(|f| f.1);

    let fix_summary: Vec<&str> = fixes.iter().map(|f| f.1).collect();
    println!("Fix automatically? Commands: {}", fix_summary.join(" && "));
    print!("[y/N] ");
    let _ = io::stdout().flush();
    let mut answer = String::new();
    let _ = io::stdin().read_line(&mut answer);
    if !answer.trim().eq_ignore_ascii_case("y") {
        println!("Skipped. You can run these commands manually.");
        return true;
    }

    println!();
    for (label, cmd) in &fixes {
        println!("Running: {cmd}  ({label})");
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        let status = std::process::Command::new(parts[0])
            .args(&parts[1..])
            .status();
        match status {
            Ok(s) if s.success() => println!("  ✅ {label} updated"),
            Ok(s) => println!("  ❌ {label} failed (exit {})", s.code().unwrap_or(-1)),
            Err(e) => println!("  ❌ {label} failed: {e}"),
        }
        println!();
    }

    println!("Re-run `lingclaw doctor` to verify.");
    true
}

fn handle_help_command() -> bool {
    println!("🦀 LingClaw v{VERSION} — Personal AI Assistant");
    println!();
    println!("Usage: lingclaw <command> [options]");
    println!();
    println!("Commands:");
    println!("  start              Start the daemon");
    println!("  stop               Stop the daemon");
    println!("  restart            Restart the daemon");
    println!("  mcp-check          Check MCP servers with runtime timeouts");
    println!("  health             Health check (exit 0 = ok)");
    println!("  status             Show detailed service status");
    println!("  update             Check for updates, rebuild if newer");
    println!("  doctor             Check install readiness (Rust, versions)");
    println!("  install [-d DIR]   Install from local source directory");
    #[cfg(not(target_os = "windows"))]
    println!("  systemd-install    Install and enable lingclaw.service");
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

fn handle_install_command(port_override: Option<u16>) -> bool {
    let args: Vec<String> = std::env::args().collect();
    let source_dir = args
        .windows(2)
        .find(|w| w[0] == "-d")
        .map(|w| PathBuf::from(&w[1]))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let cargo_toml = source_dir.join("Cargo.toml");
    if !cargo_toml.exists() {
        eprintln!("ERROR: Cargo.toml not found in {}", source_dir.display());
        eprintln!("Use `lingclaw install -d <project-dir>` to specify the source directory.");
        return true;
    }
    let cargo_content = match std::fs::read_to_string(&cargo_toml) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ERROR: Cannot read Cargo.toml: {e}");
            return true;
        }
    };
    if !cargo_content.contains("name = \"lingclaw\"") {
        eprintln!("ERROR: {} is not a LingClaw project.", source_dir.display());
        return true;
    }

    let source_version = cargo_content
        .lines()
        .find_map(|line| {
            let line = line.trim();
            if line.starts_with("version") {
                line.split('"').nth(1).map(|v| v.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "0.0.0".to_string());

    println!("Source version:    v{source_version}");
    println!("Installed version: v{VERSION}");

    let src_parts: Vec<u32> = source_version
        .split('.')
        .filter_map(|s| s.parse().ok())
        .collect();
    let cur_parts: Vec<u32> = VERSION.split('.').filter_map(|s| s.parse().ok()).collect();
    let cmp = src_parts.cmp(&cur_parts);

    match cmp {
        std::cmp::Ordering::Less => {
            eprintln!(
                "❌ Source version v{source_version} is older than installed v{VERSION}. Cannot install."
            );
            return true;
        }
        std::cmp::Ordering::Equal => {
            print!("Already at v{VERSION}. Reinstall? [y/N] ");
            let _ = io::stdout().flush();
            let mut answer = String::new();
            let _ = io::stdin().read_line(&mut answer);
            if !answer.trim().eq_ignore_ascii_case("y") {
                println!("Cancelled.");
                return true;
            }
        }
        std::cmp::Ordering::Greater => {
            print!("Upgrade v{VERSION} → v{source_version}? [y/N] ");
            let _ = io::stdout().flush();
            let mut answer = String::new();
            let _ = io::stdin().read_line(&mut answer);
            if !answer.trim().eq_ignore_ascii_case("y") {
                println!("Cancelled.");
                return true;
            }
        }
    }

    let config = Config::load();
    let check_port = port_override.unwrap_or(config.port);
    let was_running =
        std::net::TcpStream::connect_timeout(&loopback_addr(check_port), Duration::from_secs(2))
            .is_ok();
    if was_running {
        println!("Stopping service...");
        handle_stop_command(port_override);
        let exe = std::env::current_exe().ok();
        for _ in 0..10 {
            if let Some(ref path) = exe
                && std::fs::OpenOptions::new().write(true).open(path).is_ok()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    println!("Building v{source_version}...");
    let old_exe = rename_target_exe_for_build(&source_dir);
    let build = std::process::Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&source_dir)
        .status();
    match build {
        Ok(s) if s.success() => {
            if let Some(ref p) = old_exe {
                let _ = std::fs::remove_file(p);
            }
            let built_exe = source_dir
                .join("target")
                .join("release")
                .join(if cfg!(windows) {
                    "lingclaw.exe"
                } else {
                    "lingclaw"
                });
            if let Ok(current_exe) = std::env::current_exe() {
                if built_exe != current_exe {
                    match install_built_binary(&built_exe, &current_exe) {
                        Ok(_) => println!(
                            "   ✅ Installed v{source_version} → {}",
                            current_exe.display()
                        ),
                        Err(e) => {
                            eprintln!("   ❌ Failed to copy binary: {e}");
                            if was_running {
                                handle_start_command(port_override);
                            }
                            return true;
                        }
                    }
                    if let Some(install_dir) = current_exe.parent() {
                        match install_frontend_assets(&source_dir, install_dir) {
                            Ok(()) => println!(
                                "   ✅ Frontend assets installed → {}",
                                install_dir.join("static").display()
                            ),
                            Err(e) => {
                                eprintln!("   ❌ Failed to install frontend assets: {e}");
                                if was_running {
                                    handle_start_command(port_override);
                                }
                                return true;
                            }
                        }
                    }
                } else {
                    println!("   ✅ Build complete (v{source_version})");
                }
            } else {
                println!("   ✅ Build complete (v{source_version})");
            }
            match install_system_skills(&source_dir) {
                Ok(()) => println!("   ✅ System skills installed"),
                Err(e) => eprintln!("   ⚠ Failed to install system skills: {e}"),
            }
            match install_system_agents(&source_dir) {
                Ok(()) => println!("   ✅ System agents installed"),
                Err(e) => eprintln!("   ⚠ Failed to install system agents: {e}"),
            }
            if was_running {
                println!("Starting service...");
                handle_start_command(port_override);
            }
        }
        _ => {
            if let Some(ref p) = old_exe {
                let target = p.with_extension("exe");
                let _ = std::fs::rename(p, &target);
            }
            eprintln!("   ❌ Build failed");
            if was_running {
                println!("Restarting previous version...");
                handle_start_command(port_override);
            }
        }
    }
    true
}

pub(crate) fn handle_cli_command(cmd: &str, port_override: Option<u16>) -> bool {
    match cmd {
        "start" => handle_start_command(port_override),
        "stop" => handle_stop_command(port_override),
        "restart" => handle_restart_command(port_override),
        "mcp-check" => handle_mcp_check_command(),
        "health" => handle_health_command(port_override),
        "update" => handle_update_command(port_override),
        "doctor" => handle_doctor_command(),
        "status" => handle_status_command(port_override),
        "help" | "--help" | "-h" => handle_help_command(),
        "--version" | "-V" => {
            println!("lingclaw v{VERSION}");
            true
        }
        "path-install" => {
            install_global_path();
            true
        }
        #[cfg(not(target_os = "windows"))]
        "systemd-install" => install_systemd_service(),
        "install" => handle_install_command(port_override),
        _ => false,
    }
}

// ── Setup Wizard ─────────────────────────────────────────────────────────────

pub(crate) fn run_setup_wizard(force: bool) -> bool {
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
            eprintln!(
                "WARNING: Failed to backup config to {}: {e}",
                bak_path.display()
            );
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
    let provider_choice = prompt_choice(&["OpenAI", "Anthropic", "Ollama", "Skip for now"]);

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
            let api_key = prompt_secret("  API Key: ");
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
            let api_key = prompt_secret("  API Key: ");
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
        2 => {
            // Ollama
            println!();
            let base_url = prompt_line("  Base URL [http://127.0.0.1:11434]: ");
            let base_url = if base_url.is_empty() {
                "http://127.0.0.1:11434".to_string()
            } else {
                base_url
            };
            let api_key = prompt_secret("  API Key (optional, leave empty for local Ollama): ");
            providers.insert(
                "ollama".to_string(),
                json!({
                    "baseUrl": base_url,
                    "apiKey": api_key,
                    "api": "ollama",
                    "models": []
                }),
            );
            default_model = Some("ollama/qwen3".to_string());
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
        let Some(prov_name) = providers.keys().next().cloned() else {
            return true;
        };
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

    // ── Step 3: Optional local image uploads ────────────────────────────
    println!("3. Optional local image uploads (S3-compatible)");
    println!("   OpenAI/Anthropic must be able to fetch the presigned URL directly.");
    println!("   Private or localhost S3 gateways are only guaranteed with Ollama.");
    println!();
    let mut s3_config = None;
    let configure_s3 = prompt_choice(&["Configure now", "Skip for now"]);
    if configure_s3 == 0 {
        println!();
        let region = prompt_line("  Region [us-east-1]: ");
        let region = if region.is_empty() {
            "us-east-1".to_string()
        } else {
            region
        };
        let endpoint_default = crate::config::default_s3_endpoint(&region);
        let endpoint = prompt_line(&format!("  Endpoint [{endpoint_default}]: "));
        let endpoint = crate::config::normalized_s3_endpoint(
            if endpoint.is_empty() {
                None
            } else {
                Some(endpoint)
            },
            &region,
        );
        let bucket = prompt_line("  Bucket: ");
        let access_key = prompt_line("  Access key: ");
        let secret_key = prompt_secret("  Secret key: ");
        let prefix = prompt_line("  Prefix [lingclaw/images/]: ");
        let prefix = crate::config::normalized_s3_prefix(if prefix.is_empty() {
            None
        } else {
            Some(prefix)
        });
        let expiry = prompt_line("  Presigned URL expiry secs [604800]: ");
        let url_expiry_secs = expiry.parse::<u64>().unwrap_or(604_800);
        let lifecycle = prompt_line("  Lifecycle retention days [14, 0 disables]: ");
        let lifecycle_days = lifecycle.parse::<u32>().unwrap_or(14);

        if bucket.is_empty() || access_key.is_empty() || secret_key.is_empty() {
            println!(
                "   ⚠ Skipping S3 setup because bucket/access key/secret key were incomplete."
            );
        } else {
            s3_config = Some(json!({
                "endpoint": endpoint,
                "region": region,
                "bucket": bucket,
                "accessKey": access_key,
                "secretKey": secret_key,
                "prefix": prefix,
                "urlExpirySecs": url_expiry_secs,
                "lifecycleDays": lifecycle_days,
            }));
        }
    }
    println!();

    // ── Step 4: Select Channel ───────────────────────────────────────────
    println!("4. Select channel (QuickStart)");
    println!();
    let _channel = prompt_choice(&["WebChat", "Skip for now"]);
    println!();

    // ── Step 5: Global PATH ────────────────────────────────────────────
    println!("5. Do you want to add LingClaw to the global PATH?");
    println!("   This enables CLI commands: lingclaw start/stop/restart/health/update");
    println!();
    let add_path = prompt_choice(&["YES", "NO"]);
    if add_path == 0 {
        install_global_path();
    }
    println!();

    #[cfg(not(target_os = "windows"))]
    let add_systemd = {
        println!("6. Add systemd service?");
        println!("   If enabled, LingClaw will be managed by lingclaw.service.");
        println!();
        let choice = prompt_choice(&["YES", "NO"]);
        println!();
        choice == 0
    };

    // ── Step 5: Install ──────────────────────────────────────────────────
    #[cfg(target_os = "windows")]
    println!("6. Start installation");
    #[cfg(not(target_os = "windows"))]
    println!("7. Start installation");
    prompt_line("   Press Enter to continue...");
    println!();

    // Derive a sensible fast model from the chosen provider.
    let fast_model: Option<String> = match provider_choice {
        0 => Some("openai/gpt-4o-mini".to_string()),
        1 => Some("anthropic/claude-haiku-3-20250306".to_string()),
        2 => default_model.clone(),
        _ => None,
    };
    // Sub-agent model defaults to the same as fast model (cheaper for delegated tasks).
    let sub_agent_model: Option<String> = fast_model.clone();
    // Structured memory extraction also defaults to the lighter model when available.
    let memory_model: Option<String> = fast_model.clone();

    // Build config JSON
    let mut model_block = serde_json::Map::new();
    model_block.insert(
        "primary".to_string(),
        json!(default_model.unwrap_or_else(|| "gpt-4o-mini".to_string())),
    );
    if let Some(ref fast) = fast_model {
        model_block.insert("fast".to_string(), json!(fast));
    }
    if let Some(ref sub_agent) = sub_agent_model {
        model_block.insert("sub-agent".to_string(), json!(sub_agent));
    }
    if let Some(ref memory) = memory_model {
        model_block.insert("memory".to_string(), json!(memory));
    }

    let mut config = json!({
        "settings": {
            "port": DEFAULT_PORT,
            "execTimeout": 30,
            "toolTimeout": 30,
            "subAgentTimeout": 300,
            "maxLlmRetries": 2,
            "maxContextTokens": 32000,
        },
        "models": {
            "providers": providers,
        },
        "agents": {
            "defaults": {
                "model": model_block,
            }
        }
    });

    if let Some(s3_config) = s3_config {
        config["s3"] = s3_config;
    }

    // Add channel info if WebChat selected
    if _channel == 0 {
        config["channel"] = json!("webchat");
    }

    // Ensure ~/.lingclaw directory exists
    if let Some(dir) = config_dir_path()
        && let Err(e) = std::fs::create_dir_all(&dir)
    {
        eprintln!(
            "ERROR: Failed to create config directory {}: {e}",
            dir.display()
        );
        return false;
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
    if fast_model.is_some() {
        println!("   💡 Fast model configured for simple first-cycle queries.");
    }
    if sub_agent_model.is_some() {
        println!("   💡 Sub-agent model configured for delegated task execution.");
    }
    if memory_model.is_some() {
        println!("   💡 Memory model configured for structured memory extraction.");
    }
    if config.get("s3").is_some() {
        println!("   💡 Local JPEG/PNG uploads configured via S3-compatible storage.");
        println!(
            "      OpenAI/Anthropic require provider-reachable URLs; private or localhost gateways are Ollama-only."
        );
    }
    #[cfg(not(target_os = "windows"))]
    if add_systemd {
        println!();
        install_systemd_service();
    }
    println!();
    true
}

#[cfg(test)]
#[path = "tests/cli_tests.rs"]
mod cli_tests;
