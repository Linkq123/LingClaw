use std::io::{self, BufRead, Write};
#[allow(unused_imports)]
use std::net::SocketAddr;
#[allow(unused_imports)]
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::{config_dir_path, config_file_path, Config, VERSION};

// ── Interactive Helpers ──────────────────────────────────────────────────────

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
        println!(
            "Invalid choice. Please enter a number between 1 and {}.",
            options.len()
        );
    }
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
                let _ = writeln!(f, "\n# LingClaw\n{export_line}");
                added = true;
            }
        }
        if added {
            println!(
                "   ✅ Added to PATH in shell config. Run `source ~/.bashrc` or restart terminal."
            );
        } else {
            println!("   ✅ Already in PATH (or no .bashrc/.zshrc found)");
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

pub(crate) fn is_default_model_row(config: &Config, provider: &str, model_id: &str) -> bool {
    let full_ref = format!("{provider}/{model_id}");
    let default_model = config.resolved_model_ref(&config.model);
    full_ref == default_model || (config.providers.is_empty() && model_id == config.model)
}

// ── CLI Subcommands ──────────────────────────────────────────────────────────

pub(crate) fn handle_cli_command(cmd: &str, port_override: Option<u16>) -> bool {
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
                let mut command = std::process::Command::new(&exe);
                command
                    .args(&extra_args)
                    .creation_flags(0x00000008) // DETACHED_PROCESS
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
        "stop" => {
            let config = Config::load();
            let port = port_override.unwrap_or(config.port);
            let loopback = SocketAddr::from(([127, 0, 0, 1], port));
            println!("Stopping LingClaw on port {port}...");

            // Try graceful shutdown first via API
            let graceful =
                std::net::TcpStream::connect_timeout(&loopback, Duration::from_secs(2)).is_ok();

            if graceful {
                // Read shutdown token from disk
                let token = config_dir_path()
                    .map(|d| d.join(format!("shutdown-{port}.token")))
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .unwrap_or_default();

                // Send POST /api/shutdown with auth token
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
                    // Wait for graceful shutdown to complete
                    for _ in 0..10 {
                        std::thread::sleep(Duration::from_millis(500));
                        if std::net::TcpStream::connect_timeout(
                            &loopback,
                            Duration::from_millis(200),
                        )
                        .is_err()
                        {
                            println!("Stopped (graceful).");
                            return true;
                        }
                    }
                    eprintln!("Graceful shutdown timed out, force-killing...");
                }
            }

            // Fallback: force-kill
            #[cfg(target_os = "windows")]
            {
                let _ = std::process::Command::new("powershell")
                    .args(["-NoProfile", "-Command", &format!(
                        "Get-NetTCPConnection -LocalPort {port} -ErrorAction SilentlyContinue | \
                         Select-Object -ExpandProperty OwningProcess -Unique | \
                         ForEach-Object {{ Stop-Process -Id $_ -Force -ErrorAction SilentlyContinue }}"
                    )])
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
                eprintln!(
                    "ERROR: Cargo.toml not found. Run `lingclaw update` from the source directory."
                );
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

            // Stop running service first to release the binary file lock
            let config = Config::load();
            let check_port = port_override.unwrap_or(config.port);
            let was_running = std::net::TcpStream::connect_timeout(
                &format!("127.0.0.1:{check_port}")
                    .parse()
                    .expect("invalid addr"),
                Duration::from_secs(2),
            )
            .is_ok();
            if was_running {
                println!("Stopping service before build...");
                handle_cli_command("stop", port_override);
                // Wait until the exe is writable (file lock released)
                let exe = std::env::current_exe().ok();
                let mut released = false;
                for i in 0..10 {
                    if let Some(ref path) = exe {
                        if std::fs::OpenOptions::new().write(true).open(path).is_ok() {
                            released = true;
                            break;
                        }
                    } else {
                        // Can't check, just wait a reasonable time
                        std::thread::sleep(Duration::from_secs(2));
                        released = true;
                        break;
                    }
                    if i < 9 {
                        std::thread::sleep(Duration::from_millis(500));
                    }
                }
                if !released {
                    eprintln!("   ❌ Failed to release binary file lock after 5s. Is the process still running?");
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
                    println!("Starting...");
                    handle_cli_command("start", port_override);
                }
                _ => {
                    if let Some(ref p) = old_exe {
                        let target = p.with_extension("exe");
                        let _ = std::fs::rename(p, &target);
                    }
                    eprintln!("   ❌ Build failed");
                    if was_running {
                        println!("Restarting previous version...");
                        handle_cli_command("start", port_override);
                    }
                }
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
            println!(
                "  Service:       {}",
                if running {
                    "✅ Running"
                } else {
                    "❌ Stopped"
                }
            );
            println!("  Address:       http://{addr}");
            println!("  Default model: {}", config.model);
            println!("  Provider:      {}", config.provider.label());
            println!("  API base:      {}", config.api_base);
            println!("  Exec timeout:  {}s", config.exec_timeout.as_secs());
            println!("  Context limit: {} tokens", config.max_context_tokens);
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

            // Collect all models across providers into a flat table
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
            println!("  install [-d DIR]   Install from local source directory");
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
        "install" => {
            // Parse -d <dir> from args; default to current directory
            let args: Vec<String> = std::env::args().collect();
            let source_dir = args
                .windows(2)
                .find(|w| w[0] == "-d")
                .map(|w| PathBuf::from(&w[1]))
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

            let cargo_toml = source_dir.join("Cargo.toml");
            if !cargo_toml.exists() {
                eprintln!("ERROR: Cargo.toml not found in {}", source_dir.display());
                eprintln!(
                    "Use `lingclaw install -d <project-dir>` to specify the source directory."
                );
                return true;
            }
            // Verify this is a LingClaw project
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

            // Read source version
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

            // Compare versions
            let src_parts: Vec<u32> = source_version
                .split('.')
                .filter_map(|s| s.parse().ok())
                .collect();
            let cur_parts: Vec<u32> = VERSION.split('.').filter_map(|s| s.parse().ok()).collect();
            let cmp = src_parts.cmp(&cur_parts);

            match cmp {
                std::cmp::Ordering::Less => {
                    eprintln!("❌ Source version v{source_version} is older than installed v{VERSION}. Cannot install.");
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

            // Stop running service to release file lock
            let config = Config::load();
            let check_port = port_override.unwrap_or(config.port);
            let was_running = std::net::TcpStream::connect_timeout(
                &format!("127.0.0.1:{check_port}")
                    .parse()
                    .expect("invalid addr"),
                Duration::from_secs(2),
            )
            .is_ok();
            if was_running {
                println!("Stopping service...");
                handle_cli_command("stop", port_override);
                let exe = std::env::current_exe().ok();
                for _ in 0..10 {
                    if let Some(ref path) = exe {
                        if std::fs::OpenOptions::new().write(true).open(path).is_ok() {
                            break;
                        }
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
                    // Copy built binary to current exe location
                    let built_exe =
                        source_dir
                            .join("target")
                            .join("release")
                            .join(if cfg!(windows) {
                                "lingclaw.exe"
                            } else {
                                "lingclaw"
                            });
                    if let Ok(current_exe) = std::env::current_exe() {
                        if built_exe != current_exe {
                            match std::fs::copy(&built_exe, &current_exe) {
                                Ok(_) => println!(
                                    "   ✅ Installed v{source_version} → {}",
                                    current_exe.display()
                                ),
                                Err(e) => {
                                    eprintln!("   ❌ Failed to copy binary: {e}");
                                    if was_running {
                                        handle_cli_command("start", port_override);
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
                                            handle_cli_command("start", port_override);
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
                    if was_running {
                        println!("Starting service...");
                        handle_cli_command("start", port_override);
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
                        handle_cli_command("start", port_override);
                    }
                }
            }
            true
        }
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
            eprintln!(
                "ERROR: Failed to create config directory {}: {e}",
                dir.display()
            );
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
