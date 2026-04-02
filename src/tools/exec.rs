use crate::{Config, check_dangerous_command, resolve_path_checked, truncate};
use std::path::Path;

// ── think ────────────────────────────────────────────────────────────────────

pub(crate) fn tool_think(args: &serde_json::Value) -> String {
    let thought = args["thought"].as_str().unwrap_or("(no thought provided)");
    format!("Thought recorded:\n{thought}\n\nProceed with your plan.")
}

// ── exec ─────────────────────────────────────────────────────────────────────

pub(crate) async fn tool_exec(
    args: &serde_json::Value,
    config: &Config,
    workspace: &Path,
) -> String {
    let command = match args["command"].as_str() {
        Some(c) => c,
        None => return "Error: 'command' parameter is required".into(),
    };

    if let Some(pattern) = check_dangerous_command(command) {
        return format!(
            "BLOCKED: Command matches dangerous pattern '{pattern}'. Refusing to execute."
        );
    }

    let work_dir = match args["working_dir"].as_str() {
        Some(dir) => match resolve_path_checked(dir, workspace) {
            Ok(path) => path,
            Err(message) => return format!("exec error: {message}"),
        },
        None => workspace.to_path_buf(),
    };

    let shell = if cfg!(windows) { "cmd" } else { "sh" };
    let flag = if cfg!(windows) { "/C" } else { "-c" };

    let result = tokio::time::timeout(
        config.exec_timeout,
        tokio::process::Command::new(shell)
            .arg(flag)
            .arg(command)
            .current_dir(&work_dir)
            .kill_on_drop(true)
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!(
                "exit code: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                output.status.code().unwrap_or(-1),
                stdout,
                stderr
            );
            truncate(&combined, config.max_output_bytes)
        }
        Ok(Err(e)) => format!("exec error: {e}"),
        Err(_) => format!(
            "exec error: command timed out ({}s)",
            config.exec_timeout.as_secs()
        ),
    }
}
