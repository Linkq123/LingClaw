use std::{
    io::Read,
    path::Path,
    process::{ExitStatus, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt as _;

use crate::{Config, truncate};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const SAFE_GIT_GLOBAL_ARGS: &[&str] = &["--no-optional-locks", "-c", "core.fsmonitor=false"];
const GIT_FINGERPRINT_TIMEOUT: Duration = Duration::from_secs(30);

struct CapturedStream {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn capture_stream<R>(mut stream: R, limit: usize) -> std::io::Result<CapturedStream>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let retained = remaining.min(count);
        bytes.extend_from_slice(&buffer[..retained]);
        truncated |= retained < count;
    }
    Ok(CapturedStream { bytes, truncated })
}

fn render_git_output(
    status: ExitStatus,
    stdout: CapturedStream,
    stderr: CapturedStream,
    limit: usize,
) -> String {
    let mut rendered = String::new();
    let stdout_text = String::from_utf8_lossy(&stdout.bytes);
    let stderr_text = String::from_utf8_lossy(&stderr.bytes);
    if !stdout_text.trim().is_empty() {
        rendered.push_str(stdout_text.trim_end());
    }
    if stdout.truncated {
        rendered.push_str("\n[stdout truncated]");
    }
    if !stderr_text.trim().is_empty() {
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(stderr_text.trim_end());
    }
    if stderr.truncated {
        rendered.push_str("\n[stderr truncated]");
    }
    if rendered.is_empty() {
        rendered = if status.success() {
            "(no output)".to_string()
        } else {
            format!("git_inspect error: git exited with {status}")
        };
    } else if !status.success() {
        rendered = format!("git_inspect error: {rendered}");
    }
    truncate(&rendered, limit)
}

pub(crate) async fn tool_git_inspect(args: &Value, config: &Config, workspace: &Path) -> String {
    let command_args = match build_git_command_args(args, workspace) {
        Ok(args) => args,
        Err(error) => return format!("git_inspect error: {error}"),
    };

    let mut command = Command::new("git");
    command
        .args(SAFE_GIT_GLOBAL_ARGS)
        .args(&command_args)
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return format!("git_inspect error: {error}"),
    };
    let Some(stdout) = child.stdout.take() else {
        return "git_inspect error: stdout pipe was unavailable".to_string();
    };
    let Some(stderr) = child.stderr.take() else {
        return "git_inspect error: stderr pipe was unavailable".to_string();
    };
    let limit = config.max_file_bytes.max(1);
    let collect = async move {
        let stdout = capture_stream(stdout, limit);
        let stderr = capture_stream(stderr, limit);
        let status = child.wait();
        let (stdout, stderr, status) = tokio::try_join!(stdout, stderr, status)?;
        Ok::<_, std::io::Error>((stdout, stderr, status))
    };
    let (stdout, stderr, status) = match tokio::time::timeout(config.tool_timeout, collect).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return format!("git_inspect error: {error}"),
        Err(_) => return "git_inspect error: command timed out".to_string(),
    };
    render_git_output(status, stdout, stderr, config.max_file_bytes)
}

pub(crate) fn inspection_fingerprint(args: &Value, workspace: &Path) -> Result<String, String> {
    inspection_fingerprint_with_timeout(args, workspace, GIT_FINGERPRINT_TIMEOUT)
}

pub(crate) fn inspection_fingerprint_with_timeout(
    args: &Value,
    workspace: &Path,
    timeout: Duration,
) -> Result<String, String> {
    inspection_fingerprint_with_control(args, workspace, timeout, None)
}

pub(crate) fn inspection_fingerprint_with_cancellation(
    args: &Value,
    workspace: &Path,
    timeout: Duration,
    cancelled: &AtomicBool,
) -> Result<String, String> {
    inspection_fingerprint_with_control(args, workspace, timeout, Some(cancelled))
}

fn inspection_fingerprint_with_control(
    args: &Value,
    workspace: &Path,
    timeout: Duration,
    cancelled: Option<&AtomicBool>,
) -> Result<String, String> {
    let command_args = build_git_command_args(args, workspace)?;
    let mut command = std::process::Command::new("git");
    command
        .args(SAFE_GIT_GLOBAL_ARGS)
        .args(&command_args)
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "git stdout pipe was unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "git stderr pipe was unavailable".to_string())?;
    let stdout_task = std::thread::spawn(move || fingerprint_stream(stdout));
    let stderr_task = std::thread::spawn(move || fingerprint_stream(stderr));
    let status = wait_for_child(&mut child, timeout.min(GIT_FINGERPRINT_TIMEOUT), cancelled);
    let stdout = stdout_task
        .join()
        .map_err(|_| "git stdout fingerprint worker panicked".to_string())?
        .map_err(|error| error.to_string())?;
    let stderr = stderr_task
        .join()
        .map_err(|_| "git stderr fingerprint worker panicked".to_string())?
        .map_err(|error| error.to_string())?;
    let status = status?;
    if !status.success() {
        return Err(format!("git exited with {status}"));
    }
    let mut digest = Sha256::new();
    digest.update(status.code().unwrap_or_default().to_le_bytes());
    digest.update(stdout.0.to_le_bytes());
    digest.update(stdout.1);
    digest.update(stderr.0.to_le_bytes());
    digest.update(stderr.1);
    Ok(format!("{:x}", digest.finalize()))
}

fn wait_for_child(
    child: &mut std::process::Child,
    timeout: Duration,
    cancelled: Option<&AtomicBool>,
) -> Result<ExitStatus, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancelled.is_some_and(|cancelled| cancelled.load(Ordering::Relaxed)) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("git fingerprint command cancelled".to_string());
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("git fingerprint command timed out".to_string());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn fingerprint_stream(mut stream: impl Read) -> std::io::Result<(u64, [u8; 32])> {
    let mut digest = Sha256::new();
    let mut length = 0_u64;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let count = stream.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        length = length.saturating_add(count as u64);
        digest.update(&buffer[..count]);
    }
    Ok((length, digest.finalize().into()))
}

fn build_git_command_args(args: &Value, workspace: &Path) -> Result<Vec<String>, String> {
    let operation = args
        .get("operation")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut command_args = match operation {
        "status" => vec![
            "status".to_string(),
            "--short".to_string(),
            "--branch".to_string(),
        ],
        "diff" => {
            let mut values = vec![
                "diff".to_string(),
                "--no-ext-diff".to_string(),
                "--no-textconv".to_string(),
            ];
            if args.get("staged").and_then(Value::as_bool).unwrap_or(false) {
                values.push("--cached".to_string());
            }
            values
        }
        "log" => {
            let count = args
                .get("max_entries")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 100);
            vec![
                "log".to_string(),
                "--oneline".to_string(),
                "--decorate".to_string(),
                "-n".to_string(),
                count.to_string(),
            ]
        }
        "show" => {
            let reference = args
                .get("ref")
                .and_then(Value::as_str)
                .unwrap_or("HEAD")
                .trim();
            if reference.is_empty()
                || reference.starts_with('-')
                || reference.starts_with('^')
                || reference.len() > 512
                || reference.chars().any(char::is_whitespace)
                || reference.contains(':')
                || reference.contains("..")
                || reference.contains('{')
                || reference.contains('}')
                || reference.contains("^@")
                || reference.contains("^!")
                || reference.contains("^-")
            {
                return Err("invalid ref".to_string());
            }
            // Force the accepted revision to peel to exactly one commit. Git's
            // extended revision syntax can otherwise turn `show` into an
            // arbitrary object reader (`REV:path`, `REV^{tree}`) or a revision
            // set, none of which is constrained by the trailing pathspec.
            let commit = format!("{reference}^{{commit}}");
            vec![
                "show".to_string(),
                "--no-ext-diff".to_string(),
                "--no-textconv".to_string(),
                "--stat".to_string(),
                "--oneline".to_string(),
                commit,
            ]
        }
        _ => {
            return Err("operation must be status, diff, log, or show".to_string());
        }
    };

    // Git discovers repositories by walking parent directories. Always add a
    // pathspec, even when the caller omits `path`, so a Session workspace nested
    // in a larger repository cannot expose sibling files or repository-wide
    // history to Plan Mode.
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = crate::tools::safety::resolve_path_checked(path, workspace)?;
    let root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let relative = match resolved.strip_prefix(&root) {
        Ok(path) => path.to_string_lossy().replace('\\', "/"),
        Err(_) => return Err("path is outside the workspace".to_string()),
    };
    command_args.push("--".to_string());
    command_args.push(if relative.is_empty() {
        ".".to_string()
    } else {
        relative
    });

    Ok(command_args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lingclaw-git-inspect-{label}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn git_inspect_accepts_only_the_four_fixed_operations() {
        let root = workspace("operation");
        let error = build_git_command_args(&serde_json::json!({"operation":"fetch"}), &root)
            .expect_err("mutating operations must be rejected");
        assert!(error.contains("status, diff, log, or show"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn git_inspect_rejects_option_injection_in_show_ref() {
        let root = workspace("ref");
        for reference in [
            "--help",
            "HEAD --format=%B",
            "HEAD\n--help",
            "",
            "HEAD:Cargo.toml",
            "HEAD^{tree}",
            "HEAD..main",
            "^HEAD",
            "HEAD^@",
        ] {
            assert_eq!(
                build_git_command_args(
                    &serde_json::json!({"operation":"show", "ref":reference}),
                    &root,
                ),
                Err("invalid ref".to_string())
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn git_inspect_show_forces_a_single_commit_object() {
        let root = workspace("commit-ref");
        let args = build_git_command_args(
            &serde_json::json!({"operation":"show", "ref":"feature/topic~2"}),
            &root,
        )
        .expect("a normal commit-ish should be accepted");

        assert!(args.iter().any(|arg| arg == "feature/topic~2^{commit}"));
        assert_eq!(&args[args.len() - 2..], ["--", "."]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn git_inspect_rejects_paths_outside_the_workspace() {
        let root = workspace("path");
        let error = build_git_command_args(
            &serde_json::json!({"operation":"diff", "path":"../outside"}),
            &root,
        )
        .expect_err("path traversal must be rejected");
        assert!(error.contains("outside") || error.contains("traversal"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn git_inspect_builds_bounded_log_arguments_without_a_shell() {
        let root = workspace("log");
        let args = build_git_command_args(
            &serde_json::json!({"operation":"log", "max_entries":1000}),
            &root,
        )
        .unwrap();
        assert_eq!(
            args,
            ["log", "--oneline", "--decorate", "-n", "100", "--", "."]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn git_inspect_scopes_default_operations_to_the_workspace() {
        let root = workspace("default-pathspec");
        for operation in ["status", "diff", "log", "show"] {
            let args = build_git_command_args(&serde_json::json!({"operation":operation}), &root)
                .expect("fixed Git inspection operations should be accepted");
            assert_eq!(
                &args[args.len() - 2..],
                ["--", "."],
                "{operation} must never fall back to the parent repository scope"
            );
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn git_inspect_disables_configured_process_hooks_and_text_converters() {
        assert_eq!(
            SAFE_GIT_GLOBAL_ARGS,
            ["--no-optional-locks", "-c", "core.fsmonitor=false"]
        );

        let root = workspace("safe-config");
        for operation in ["diff", "show"] {
            let args = build_git_command_args(&serde_json::json!({"operation": operation}), &root)
                .unwrap();
            assert!(args.iter().any(|arg| arg == "--no-ext-diff"));
            assert!(args.iter().any(|arg| arg == "--no-textconv"));
        }
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn git_inspect_drains_output_while_retaining_only_the_configured_prefix() {
        let input = vec![b'x'; 8 * 1024];
        let captured = capture_stream(input.as_slice(), 128)
            .await
            .expect("in-memory output should be readable");

        assert_eq!(captured.bytes, vec![b'x'; 128]);
        assert!(captured.truncated);
    }
}
