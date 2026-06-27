use crate::Config;
use crate::tools::safety::{check_dangerous_command, resolve_path_checked};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::error::TrySendError;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;
const EXEC_CAPTURE_OVERHEAD_BYTES: usize = 256;
const EXEC_READ_CHUNK_BYTES: usize = 8192;
const EXEC_LIVE_EVENT_MAX_CHARS: usize = 4000;
pub(crate) const REDACTED_VALUE: &str = "[REDACTED]";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExecRequest {
    command: Option<String>,
    program: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    working_dir: Option<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    stdin: Option<String>,
}

#[derive(Debug, Clone)]
enum ExecMode {
    Shell { command: String },
    Direct { program: String, args: Vec<String> },
}

#[derive(Debug, Clone)]
struct ExecRequest {
    mode: ExecMode,
    working_dir: PathBuf,
    env: BTreeMap<String, String>,
    stdin: Option<Vec<u8>>,
}

#[derive(Debug)]
enum ExecRunOutcome {
    Completed {
        status: std::process::ExitStatus,
        stdout: StreamCapture,
        stderr: StreamCapture,
    },
    TimedOut {
        stdout: StreamCapture,
        stderr: StreamCapture,
    },
}

#[derive(Debug, Clone)]
struct SharedByteBudget {
    remaining: Arc<AtomicUsize>,
}

impl SharedByteBudget {
    fn new(limit: usize) -> Self {
        Self {
            remaining: Arc::new(AtomicUsize::new(limit)),
        }
    }

    fn take(&self, requested: usize) -> usize {
        if requested == 0 {
            return 0;
        }

        let mut remaining = self.remaining.load(Ordering::Relaxed);
        loop {
            if remaining == 0 {
                return 0;
            }

            let granted = remaining.min(requested);
            match self.remaining.compare_exchange_weak(
                remaining,
                remaining - granted,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return granted,
                Err(updated) => remaining = updated,
            }
        }
    }
}

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
    event_tx: Option<super::ToolEventSender>,
    bounded_event_tx: Option<super::BoundedToolEventSender>,
) -> super::ToolHandlerOutput {
    let request = match parse_exec_request(args, workspace) {
        Ok(request) => request,
        Err(message) => {
            return super::ToolHandlerOutput::explicit(format!("exec error: {message}"), true);
        }
    };

    if let Some(pattern) = check_dangerous_command(&request.policy_preview()) {
        return super::ToolHandlerOutput::explicit(
            format!("BLOCKED: Command matches dangerous pattern '{pattern}'. Refusing to execute."),
            true,
        );
    }

    match run_exec_request(
        &request,
        config.exec_timeout,
        config.max_output_bytes,
        event_tx,
        bounded_event_tx,
    )
    .await
    {
        Ok(ExecRunOutcome::Completed {
            status,
            stdout,
            stderr,
        }) => {
            let exit_code = format_exit_code(&status);
            let is_error = !status.success();
            let output = truncate_exec_output(
                format_exec_output(&exit_code, &stdout, &stderr, is_error),
                config.max_output_bytes,
            );
            super::ToolHandlerOutput::explicit(output, is_error)
        }
        Ok(ExecRunOutcome::TimedOut { stdout, stderr }) => super::ToolHandlerOutput::explicit(
            truncate_exec_output(
                format_exec_timeout_output(config.exec_timeout, &stdout, &stderr),
                config.max_output_bytes,
            ),
            true,
        ),
        Err(error) => super::ToolHandlerOutput::explicit(format!("exec error: {error}"), true),
    }
}

pub(crate) fn summarize_exec_request(args: &serde_json::Value) -> Option<String> {
    let request = RawExecRequest::deserialize(args.clone()).ok()?;
    let preview = match normalized_mode(request.command.as_deref(), request.program.as_deref()) {
        Ok(ExecModeSelector::Shell) => request.command.as_deref()?,
        Ok(ExecModeSelector::Direct) => {
            return Some(sanitize_exec_command_for_display(&join_argv(
                request.program.as_deref()?,
                &request.args,
            )));
        }
        Err(_) => return None,
    };
    Some(sanitize_exec_command_for_display(preview))
}

pub(crate) fn sanitize_exec_command_for_display(command: &str) -> String {
    exec_redaction_patterns()
        .iter()
        .fold(command.to_string(), |current, regex| {
            regex
                .replace_all(&current, |captures: &regex::Captures| {
                    format!(
                        "{}{}{}",
                        captures.get(1).map_or("", |m| m.as_str()),
                        REDACTED_VALUE,
                        captures.get(3).map_or("", |m| m.as_str())
                    )
                })
                .into_owned()
        })
}

fn exec_redaction_patterns() -> &'static [regex::Regex] {
    static PATTERNS: std::sync::OnceLock<Vec<regex::Regex>> = std::sync::OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            vec![
                regex::Regex::new(r#"(?i)(authorization\s*[:=]\s*bearer\s+")([^"]+)(")"#)
                    .expect("authorization bearer regex should compile"),
                regex::Regex::new(r#"(?i)(authorization\s*[:=]\s*bearer\s+')([^']+)(')"#)
                    .expect("authorization bearer regex should compile"),
                regex::Regex::new(r#"(?i)(authorization\s*[:=]\s*bearer\s+)([^\s"'`]+)"#)
                    .expect("authorization bearer regex should compile"),
                regex::Regex::new(
                    r#"(?i)((?:--?(?:api[_-]?key|token|access[_-]?token|password|passwd|secret))(?:=|\s+)")([^"]+)(")"#,
                )
                .expect("flag-like secret regex should compile"),
                regex::Regex::new(
                    r#"(?i)((?:--?(?:api[_-]?key|token|access[_-]?token|password|passwd|secret))(?:=|\s+)')([^']+)(')"#,
                )
                .expect("flag-like secret regex should compile"),
                regex::Regex::new(
                    r#"(?i)((?:--?(?:api[_-]?key|token|access[_-]?token|password|passwd|secret))(?:=|\s+))([^\s"'`]+)"#,
                )
                .expect("flag-like secret regex should compile"),
                regex::Regex::new(
                    r#"(?i)\b((?:api[_-]?key|token|access[_-]?token|password|passwd|secret)\s*=\s*")([^"]+)(")"#,
                )
                .expect("assignment secret regex should compile"),
                regex::Regex::new(
                    r#"(?i)\b((?:api[_-]?key|token|access[_-]?token|password|passwd|secret)\s*=\s*')([^']+)(')"#,
                )
                .expect("assignment secret regex should compile"),
                regex::Regex::new(
                    r#"(?i)\b((?:api[_-]?key|token|access[_-]?token|password|passwd|secret)\s*=\s*)([^\s"'`]+)"#,
                )
                .expect("assignment secret regex should compile"),
            ]
        })
        .as_slice()
}

fn parse_exec_request(args: &serde_json::Value, workspace: &Path) -> Result<ExecRequest, String> {
    let raw = RawExecRequest::deserialize(args.clone())
        .map_err(|error| format!("invalid arguments: {error}"))?;
    raw.into_exec_request(workspace)
}

impl RawExecRequest {
    fn into_exec_request(self, workspace: &Path) -> Result<ExecRequest, String> {
        let mode = match normalized_mode(self.command.as_deref(), self.program.as_deref())? {
            ExecModeSelector::Shell => {
                if !self.args.is_empty() {
                    return Err("parameter 'args' can only be used with 'program'".to_string());
                }
                ExecMode::Shell {
                    command: require_non_blank(self.command, "command")?,
                }
            }
            ExecModeSelector::Direct => ExecMode::Direct {
                program: require_non_blank(self.program, "program")?,
                args: self.args,
            },
        };

        let working_dir = match self.working_dir {
            Some(dir) => resolve_path_checked(&dir, workspace)
                .map_err(|message| format!("working_dir {message}"))?,
            None => workspace.to_path_buf(),
        };
        validate_exec_env(&self.env)?;

        Ok(ExecRequest {
            mode,
            working_dir,
            env: self.env,
            stdin: self.stdin.map(String::into_bytes),
        })
    }
}

enum ExecModeSelector {
    Shell,
    Direct,
}

fn normalized_mode(
    command: Option<&str>,
    program: Option<&str>,
) -> Result<ExecModeSelector, String> {
    match (
        command.map(str::trim).filter(|value| !value.is_empty()),
        program.map(str::trim).filter(|value| !value.is_empty()),
    ) {
        (Some(_), None) => Ok(ExecModeSelector::Shell),
        (None, Some(_)) => Ok(ExecModeSelector::Direct),
        (Some(_), Some(_)) => Err("use either 'command' or 'program', not both".to_string()),
        (None, None) => {
            Err("missing required parameter: provide either 'command' or 'program'".to_string())
        }
    }
}

fn require_non_blank(value: Option<String>, key: &str) -> Result<String, String> {
    let Some(value) = value else {
        return Err(format!("missing required parameter '{key}'"));
    };
    if value.trim().is_empty() {
        return Err(format!("parameter '{key}' cannot be blank"));
    }
    Ok(value)
}

fn validate_exec_env(env: &BTreeMap<String, String>) -> Result<(), String> {
    for (key, value) in env {
        if key.is_empty() {
            return Err("environment variable names cannot be empty".to_string());
        }
        if key.contains('=') {
            return Err(format!("environment variable '{key}' cannot contain '='"));
        }
        if key.contains('\0') {
            return Err(format!(
                "environment variable '{key}' cannot contain a NUL byte"
            ));
        }
        if value.contains('\0') {
            return Err(format!(
                "environment variable '{key}' cannot contain a NUL byte in its value"
            ));
        }
    }
    Ok(())
}

impl ExecRequest {
    fn policy_preview(&self) -> String {
        match &self.mode {
            ExecMode::Shell { command } => command.clone(),
            ExecMode::Direct { program, args } => join_argv(program, args),
        }
    }
}

fn join_argv(program: &str, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(quote_arg_for_display(program));
    parts.extend(args.iter().map(|arg| quote_arg_for_display(arg)));
    parts.join(" ")
}

fn quote_arg_for_display(arg: &str) -> String {
    if arg.is_empty() {
        return "\"\"".to_string();
    }
    if arg
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "-_./:\\".contains(ch))
    {
        return arg.to_string();
    }
    format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
}

async fn run_exec_request(
    request: &ExecRequest,
    timeout: Duration,
    max_output_bytes: usize,
    event_tx: Option<super::ToolEventSender>,
    bounded_event_tx: Option<super::BoundedToolEventSender>,
) -> std::io::Result<ExecRunOutcome> {
    let capture_limit = capture_budget_limit(max_output_bytes);
    let live_budget = SharedByteBudget::new(max_output_bytes);
    let mut command_process = build_command(request);

    let mut child = command_process.spawn()?;
    let stdin_writer = child
        .stdin
        .take()
        .zip(request.stdin.clone())
        .map(|(stdin, bytes)| tokio::spawn(write_stdin(stdin, bytes)));
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("failed to capture command stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("failed to capture command stderr"))?;

    let stdout_task = tokio::spawn(capture_stream(
        "stdout",
        stdout,
        capture_limit,
        live_budget.clone(),
        event_tx.clone(),
        bounded_event_tx.clone(),
    ));
    let stderr_task = tokio::spawn(capture_stream(
        "stderr",
        stderr,
        capture_limit,
        live_budget,
        event_tx,
        bounded_event_tx,
    ));

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(wait_result) => Some(wait_result?),
        Err(_) => {
            terminate_child(&mut child).await?;
            None
        }
    };

    if let Some(stdin_writer) = stdin_writer {
        stdin_writer
            .await
            .map_err(join_error_to_io)?
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::BrokenPipe {
                    Ok(())
                } else {
                    Err(error)
                }
            })?;
    }

    let stdout = stdout_task.await.map_err(join_error_to_io)??;
    let stderr = stderr_task.await.map_err(join_error_to_io)??;

    let Some(status) = status else {
        return Ok(ExecRunOutcome::TimedOut { stdout, stderr });
    };
    Ok(ExecRunOutcome::Completed {
        status,
        stdout,
        stderr,
    })
}

fn build_command(request: &ExecRequest) -> tokio::process::Command {
    let mut command_process = match &request.mode {
        ExecMode::Shell { command } => {
            let shell = if cfg!(windows) { "cmd" } else { "sh" };
            let flag = if cfg!(windows) { "/C" } else { "-c" };
            let mut process = tokio::process::Command::new(shell);
            process.arg(flag).arg(command);
            process
        }
        ExecMode::Direct { program, args } => {
            let mut process = tokio::process::Command::new(program);
            process.args(args);
            process
        }
    };

    command_process
        .current_dir(&request.working_dir)
        .envs(request.env.iter())
        .stdin(if request.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    apply_exec_process_flags(&mut command_process);
    command_process
}

async fn terminate_child(child: &mut tokio::process::Child) -> std::io::Result<()> {
    if let Err(error) = child.kill().await
        && error.kind() != std::io::ErrorKind::InvalidInput
    {
        return Err(error);
    }
    let _ = child.wait().await;
    Ok(())
}

async fn write_stdin(mut stdin: tokio::process::ChildStdin, bytes: Vec<u8>) -> std::io::Result<()> {
    stdin.write_all(&bytes).await?;
    stdin.shutdown().await
}

fn join_error_to_io(error: tokio::task::JoinError) -> std::io::Error {
    std::io::Error::other(format!("exec task join error: {error}"))
}

fn capture_budget_limit(max_output_bytes: usize) -> usize {
    max_output_bytes
        .saturating_sub(EXEC_CAPTURE_OVERHEAD_BYTES)
        .max(2)
}

fn format_exit_code(status: &std::process::ExitStatus) -> String {
    status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "terminated by signal".to_string())
}

fn truncate_exec_output(mut output: String, max_output_bytes: usize) -> String {
    if output.len() <= max_output_bytes {
        return output;
    }

    const SUFFIX: &str = "\n[truncated]";
    if max_output_bytes == 0 {
        return String::new();
    }
    if SUFFIX.len() >= max_output_bytes {
        let mut end = max_output_bytes;
        while end > 0 && !output.is_char_boundary(end) {
            end -= 1;
        }
        output.truncate(end);
        return output;
    }

    let mut end = max_output_bytes - SUFFIX.len();
    while end > 0 && !output.is_char_boundary(end) {
        end -= 1;
    }
    output.truncate(end);
    output.push_str(SUFFIX);
    output
}

fn format_exec_output(
    exit_code: &str,
    stdout: &StreamCapture,
    stderr: &StreamCapture,
    is_error: bool,
) -> String {
    let stdout_text = stdout.render();
    let stderr_text = stderr.render();
    let mut output = String::new();
    if is_error {
        output.push_str(&format!(
            "exec error: command exited with code {exit_code}\n"
        ));
    }
    output.push_str(&format!(
        "exit code: {exit_code}\n--- stdout ---\n{stdout_text}\n--- stderr ---\n{stderr_text}"
    ));
    output
}

fn format_exec_timeout_output(
    timeout: Duration,
    stdout: &StreamCapture,
    stderr: &StreamCapture,
) -> String {
    format!(
        "exec error: command timed out ({}s)\n--- stdout ---\n{}\n--- stderr ---\n{}",
        timeout.as_secs(),
        stdout.render(),
        stderr.render()
    )
}

const EXEC_CAPTURE_TRUNCATION_PREFIX: &str = "[truncated]\n";

#[derive(Debug)]
struct StreamCapture {
    label: &'static str,
    captured: Vec<u8>,
    total_bytes: usize,
}

impl StreamCapture {
    fn render(&self) -> String {
        let mut output = String::from_utf8_lossy(&self.captured).into_owned();
        if self.total_bytes > self.captured.len() {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&format!(
                "[{} truncated at {} bytes, total {} bytes]",
                self.label,
                self.captured.len(),
                self.total_bytes
            ));
        }
        output
    }
}

fn truncate_capture_tail(bytes: &mut Vec<u8>, max: usize) {
    if bytes.len() <= max {
        return;
    }

    let prefix = EXEC_CAPTURE_TRUNCATION_PREFIX.as_bytes();
    if prefix.len() >= max {
        bytes.truncate(max);
        return;
    }

    let keep = max - prefix.len();
    let mut start = bytes.len().saturating_sub(keep);
    while start < bytes.len() && std::str::from_utf8(&bytes[start..]).is_err() {
        start += 1;
    }

    let mut truncated = prefix.to_vec();
    truncated.extend_from_slice(&bytes[start..]);
    *bytes = truncated;
}

fn extend_live_forward_to_utf8_boundary(
    bytes: &[u8],
    forwarded: usize,
    pending_utf8: &[u8],
    live_budget: &SharedByteBudget,
) -> usize {
    let mut end = forwarded.min(bytes.len());
    if end == 0 {
        return 0;
    }

    let mut combined = Vec::new();
    loop {
        combined.clear();
        combined.extend_from_slice(pending_utf8);
        combined.extend_from_slice(&bytes[..end]);
        let (complete, tail) = split_incomplete_utf8_suffix(&combined);
        let complete_from_current = complete.len().saturating_sub(pending_utf8.len()).min(end);
        if tail.is_empty() || end >= bytes.len() {
            return complete_from_current;
        }
        if live_budget.take(1) == 0 {
            return complete_from_current;
        }
        end += 1;
    }
}

fn flush_pending_live_utf8(
    stream: &'static str,
    pending_utf8: &mut Vec<u8>,
    event_tx: Option<super::ToolEventSender>,
    bounded_event_tx: Option<super::BoundedToolEventSender>,
) {
    if pending_utf8.is_empty() {
        return;
    }

    let bytes = std::mem::take(pending_utf8);
    let text = String::from_utf8_lossy(&bytes);
    emit_live_text_chunks(stream, &text, event_tx, bounded_event_tx);
}

async fn capture_stream<R>(
    label: &'static str,
    mut reader: R,
    capture_limit: usize,
    live_budget: SharedByteBudget,
    event_tx: Option<super::ToolEventSender>,
    bounded_event_tx: Option<super::BoundedToolEventSender>,
) -> std::io::Result<StreamCapture>
where
    R: AsyncRead + Unpin,
{
    let mut captured = Vec::new();
    let mut total_bytes = 0usize;
    let mut buffer = [0_u8; EXEC_READ_CHUNK_BYTES];
    let mut pending_live_utf8 = Vec::new();

    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        total_bytes += read;

        let mut forwarded = live_budget.take(read);
        if forwarded > 0 {
            forwarded = extend_live_forward_to_utf8_boundary(
                &buffer[..read],
                forwarded,
                &pending_live_utf8,
                &live_budget,
            );
            forward_live_chunk(
                label,
                &buffer[..forwarded],
                &mut pending_live_utf8,
                event_tx.clone(),
                bounded_event_tx.clone(),
            );
        }

        captured.extend_from_slice(&buffer[..read]);
        truncate_capture_tail(&mut captured, capture_limit);
    }

    flush_pending_live_utf8(label, &mut pending_live_utf8, event_tx, bounded_event_tx);

    Ok(StreamCapture {
        label,
        captured,
        total_bytes,
    })
}

fn split_incomplete_utf8_suffix(bytes: &[u8]) -> (&[u8], &[u8]) {
    match std::str::from_utf8(bytes) {
        Ok(_) => (bytes, &[]),
        Err(error) if error.error_len().is_none() => {
            let valid_up_to = error.valid_up_to();
            (&bytes[..valid_up_to], &bytes[valid_up_to..])
        }
        Err(_) => (bytes, &[]),
    }
}

fn emit_live_text_chunks(
    stream: &'static str,
    chunk: &str,
    event_tx: Option<super::ToolEventSender>,
    bounded_event_tx: Option<super::BoundedToolEventSender>,
) {
    if chunk.is_empty() {
        return;
    }

    let mut start = 0usize;
    while start < chunk.len() {
        let mut end = (start + EXEC_LIVE_EVENT_MAX_CHARS).min(chunk.len());
        while end > start && !chunk.is_char_boundary(end) {
            end -= 1;
        }
        if end == start {
            break;
        }
        let event = super::ToolLiveEvent::ExecOutput {
            stream,
            chunk: chunk[start..end].to_string(),
        };
        let sent = if let Some(event_tx) = event_tx.as_ref() {
            event_tx.send(event).is_ok()
        } else if let Some(event_tx) = bounded_event_tx.as_ref() {
            match event_tx.try_send(event) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => false,
                Err(TrySendError::Closed(_)) => false,
            }
        } else {
            false
        };
        if !sent {
            break;
        }
        start = end;
    }
}

pub(crate) fn forward_live_chunk(
    stream: &'static str,
    bytes: &[u8],
    pending_utf8: &mut Vec<u8>,
    event_tx: Option<super::ToolEventSender>,
    bounded_event_tx: Option<super::BoundedToolEventSender>,
) {
    if bytes.is_empty() {
        return;
    }

    let mut combined = std::mem::take(pending_utf8);
    combined.extend_from_slice(bytes);
    let (complete, tail) = split_incomplete_utf8_suffix(&combined);
    if !tail.is_empty() {
        pending_utf8.extend_from_slice(tail);
    }
    if complete.is_empty() {
        return;
    }

    let text = String::from_utf8_lossy(complete);
    emit_live_text_chunks(stream, &text, event_tx, bounded_event_tx);
}

fn apply_exec_process_flags(command: &mut tokio::process::Command) {
    #[cfg(target_os = "windows")]
    {
        // Prevent `cmd.exe` from flashing a transient console window for each
        // exec tool invocation when LingClaw is launched outside a terminal.
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(target_os = "windows"))]
    let _ = command;
}
