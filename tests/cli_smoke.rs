use std::{
    collections::HashMap,
    convert::Infallible,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::http::StatusCode;
use axum::{Json, Router, body::Body, extract::State, response::IntoResponse, routing::post};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::{Mutex, Notify};
use tokio_tungstenite::tungstenite::Message;

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
const SHORT_PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const HEALTH_IO_POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_HEALTH_RESPONSE_BYTES: usize = 64 * 1024;

struct ChildGuard {
    child: Child,
    home: PathBuf,
    port: u16,
    description: String,
    cleanup_report: Option<CleanupReport>,
}

#[derive(Clone, Copy, Debug)]
struct CleanupReport {
    child_reaped: bool,
    home_removed: bool,
}

#[derive(Debug)]
struct CapturedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    home: PathBuf,
    cleanup: CleanupReport,
}

#[derive(Debug)]
struct ProcessTimeout {
    description: String,
    home: PathBuf,
    cleanup: CleanupReport,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

impl ChildGuard {
    fn cleanup(&mut self) -> CleanupReport {
        if let Some(report) = self.cleanup_report {
            return report;
        }

        let child_reaped = match self.child.try_wait() {
            Ok(Some(_)) => true,
            Ok(None) | Err(_) => {
                let _ = self.child.kill();
                let _ = self.child.stdin.take();
                self.child.wait().is_ok()
            }
        };
        let home_removed = match std::fs::remove_dir_all(&self.home) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        };
        let report = CleanupReport {
            child_reaped,
            home_removed,
        };
        self.cleanup_report = Some(report);
        report
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) => {}
                Err(error) => panic!("failed to inspect {}: {error}", self.description),
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            thread::sleep(remaining.min(PROCESS_POLL_INTERVAL));
        }
    }

    fn wait_for_output(mut self, timeout: Duration) -> Result<CapturedOutput, ProcessTimeout> {
        let status = match self.wait_for_exit(timeout) {
            Some(status) => status,
            None => {
                let cleanup = self.cleanup();
                let timed_out = ProcessTimeout {
                    description: self.description.clone(),
                    home: self.home.clone(),
                    cleanup,
                };
                return Err(timed_out);
            }
        };

        let mut stdout = Vec::new();
        self.child
            .stdout
            .take()
            .expect("child stdout should be piped")
            .read_to_end(&mut stdout)
            .expect("child stdout should be readable");
        let mut stderr = Vec::new();
        self.child
            .stderr
            .take()
            .expect("child stderr should be piped")
            .read_to_end(&mut stderr)
            .expect("child stderr should be readable");

        let cleanup = self.cleanup();
        let output = CapturedOutput {
            status,
            stdout,
            stderr,
            home: self.home.clone(),
            cleanup,
        };
        Ok(output)
    }

    fn terminate(mut self) -> CleanupReport {
        self.cleanup()
    }

    fn terminate_and_capture(mut self) -> CapturedOutput {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => {
                self.child.kill().expect("daemon should terminate");
                self.child.wait().expect("daemon should be reaped")
            }
            Err(error) => panic!("failed to inspect {}: {error}", self.description),
        };
        let mut stdout = Vec::new();
        self.child
            .stdout
            .take()
            .expect("child stdout should be piped")
            .read_to_end(&mut stdout)
            .expect("child stdout should be readable");
        let mut stderr = Vec::new();
        self.child
            .stderr
            .take()
            .expect("child stderr should be piped")
            .read_to_end(&mut stderr)
            .expect("child stderr should be readable");
        let cleanup = self.cleanup();
        CapturedOutput {
            status,
            stdout,
            stderr,
            home: self.home.clone(),
            cleanup,
        }
    }

    fn stop_preserving_home(&mut self) {
        let child_reaped = match self.child.try_wait() {
            Ok(Some(_)) => true,
            Ok(None) => {
                self.child.kill().expect("daemon should terminate");
                self.child.wait().is_ok()
            }
            Err(_) => false,
        };
        assert!(child_reaped, "daemon should be reaped before restart");
        self.cleanup_report = Some(CleanupReport {
            child_reaped: true,
            home_removed: false,
        });
    }
}

fn reserve_ephemeral_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port should bind");
    listener.local_addr().expect("listener address").port()
}

fn create_isolated_home(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time")
        .as_nanos();
    let home =
        std::env::temp_dir().join(format!("lingclaw-{label}-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&home).expect("isolated home should be created");
    home
}

fn spawn_lingclaw(label: &str, args: &[&str], stdin: Stdio) -> ChildGuard {
    spawn_lingclaw_prepared(label, args, stdin, |_, _| {})
}

fn spawn_lingclaw_prepared(
    label: &str,
    args: &[&str],
    stdin: Stdio,
    prepare: impl FnOnce(&Path, u16),
) -> ChildGuard {
    let port = reserve_ephemeral_port();
    let home = create_isolated_home(label);
    prepare(&home, port);
    spawn_lingclaw_in_home(home, port, args, stdin)
}

fn spawn_lingclaw_in_home(home: PathBuf, port: u16, args: &[&str], stdin: Stdio) -> ChildGuard {
    let mut command = Command::new(env!("CARGO_BIN_EXE_lingclaw"));
    command
        .args(args)
        .args(["--port", &port.to_string()])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&home);
            panic!("isolated LingClaw process should start: {error}");
        }
    };
    ChildGuard {
        child,
        home,
        port,
        description: args.join(" "),
        cleanup_report: None,
    }
}

fn assert_resources_released(home: &Path, cleanup: CleanupReport) {
    assert!(cleanup.child_reaped, "child process was not reaped");
    assert!(cleanup.home_removed, "isolated HOME cleanup failed");
    assert!(
        !home.exists(),
        "isolated HOME was not removed: {}",
        home.display()
    );
}

fn socket_timeout(deadline: Instant) -> Result<Duration, String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("health probe deadline expired".to_string());
    }
    Ok(remaining.min(HEALTH_PROBE_TIMEOUT))
}

fn wait_for_health_io(deadline: Instant) -> Result<(), String> {
    let remaining = socket_timeout(deadline)?;
    thread::sleep(remaining.min(HEALTH_IO_POLL_INTERVAL));
    Ok(())
}

fn write_health_request(stream: &mut TcpStream, deadline: Instant) -> Result<(), String> {
    let request = b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n";
    let mut written = 0;
    while written < request.len() {
        socket_timeout(deadline)?;
        match stream.write(&request[written..]) {
            Ok(0) => return Err("health connection closed while writing".to_string()),
            Ok(count) => written += count,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_for_health_io(deadline)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn read_health_response(stream: &mut TcpStream, deadline: Instant) -> Result<String, String> {
    let mut response = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        socket_timeout(deadline)?;
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                response.extend_from_slice(&buffer[..count]);
                if response.len() > MAX_HEALTH_RESPONSE_BYTES {
                    return Err("health response exceeded size limit".to_string());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_for_health_io(deadline)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    String::from_utf8(response).map_err(|error| format!("health response was not UTF-8: {error}"))
}

fn lingclaw_health(port: u16, overall_deadline: Instant) -> Result<serde_json::Value, String> {
    let probe_deadline = overall_deadline.min(Instant::now() + HEALTH_PROBE_TIMEOUT);
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, socket_timeout(probe_deadline)?)
        .map_err(|error| error.to_string())?;
    stream
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    write_health_request(&mut stream, probe_deadline)?;

    let response = read_health_response(&mut stream, probe_deadline)?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "health response had no header terminator".to_string())?;
    if !headers
        .lines()
        .next()
        .is_some_and(|line| line.contains(" 200 "))
    {
        return Err(format!("health response was not HTTP 200: {headers}"));
    }
    serde_json::from_str(body).map_err(|error| format!("invalid health JSON: {error}"))
}

#[test]
fn long_help_flag_prints_help_without_entering_setup() {
    let output = spawn_lingclaw("long-help", &["--help", "--serve"], Stdio::null())
        .wait_for_output(SHORT_PROCESS_TIMEOUT)
        .unwrap_or_else(|timeout| panic!("{} did not exit: {timeout:?}", timeout.description));

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: lingclaw <command> [options]"));
    assert!(stdout.contains("--version, -V"));
    assert_resources_released(&output.home, output.cleanup);
}

#[test]
fn short_help_flag_prints_help_without_entering_setup() {
    let output = spawn_lingclaw("short-help", &["-h", "--serve"], Stdio::null())
        .wait_for_output(SHORT_PROCESS_TIMEOUT)
        .unwrap_or_else(|timeout| panic!("{} did not exit: {timeout:?}", timeout.description));

    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("Commands:"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_resources_released(&output.home, output.cleanup);
}

#[test]
fn timed_out_process_is_killed_reaped_and_cleaned_up() {
    let process = spawn_lingclaw(
        "timeout-cleanup",
        &["--install-daemon", "--serve"],
        Stdio::piped(),
    );
    let started = Instant::now();
    let timeout = process
        .wait_for_output(Duration::from_millis(200))
        .expect_err("wizard with open stdin should still be waiting");

    assert!(
        started.elapsed() < Duration::from_secs(2),
        "bounded process wait took too long"
    );
    assert_resources_released(&timeout.home, timeout.cleanup);
}

#[test]
fn health_probe_respects_its_deadline_when_the_peer_stalls() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("ephemeral port should bind");
    let port = listener.local_addr().expect("listener address").port();
    let server = thread::spawn(move || {
        let (_stream, _) = listener.accept().expect("health probe should connect");
        thread::sleep(Duration::from_millis(600));
    });

    let started = Instant::now();
    let result = lingclaw_health(port, started + Duration::from_millis(100));
    let elapsed = started.elapsed();
    server.join().expect("stalled peer should exit");

    assert!(result.is_err(), "stalled response must not be healthy");
    assert!(
        elapsed < Duration::from_millis(500),
        "health probe exceeded its shared deadline: {elapsed:?}"
    );
}

#[test]
fn explicit_install_daemon_still_runs_the_wizard_in_serve_mode() {
    let mut process = spawn_lingclaw(
        "force-wizard",
        &["--install-daemon", "--serve"],
        Stdio::piped(),
    );
    process
        .child
        .stdin
        .take()
        .expect("wizard stdin")
        .write_all(b"2\n")
        .expect("wizard answer should be written");

    let output = process
        .wait_for_output(SHORT_PROCESS_TIMEOUT)
        .unwrap_or_else(|timeout| panic!("{} did not exit: {timeout:?}", timeout.description));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LingClaw Setup Wizard"));
    assert!(stdout.contains("Bye!"));
    assert_resources_released(&output.home, output.cleanup);
}

#[test]
fn serve_mode_with_a_clean_home_binds_without_waiting_for_the_wizard() {
    let mut daemon = spawn_lingclaw("clean-serve", &["--serve"], Stdio::null());
    let port = daemon.port;
    let home = daemon.home.clone();

    let deadline = Instant::now() + Duration::from_secs(10);
    let health = loop {
        if let Ok(payload) = lingclaw_health(port, deadline)
            && payload.get("service").and_then(|value| value.as_str()) == Some("lingclaw")
            && payload.get("status").and_then(|value| value.as_str()) == Some("ok")
        {
            break payload;
        }
        if let Some(status) = daemon.child.try_wait().expect("child status") {
            panic!("clean --serve exited before becoming healthy: {status}");
        }
        if Instant::now() >= deadline {
            panic!("clean --serve never returned a LingClaw health response");
        }
        thread::sleep(Duration::from_millis(100));
    };

    assert_eq!(health["storage"]["mode"], "healthy");
    assert!(
        daemon.child.try_wait().expect("child status").is_none(),
        "daemon exited immediately after its health response"
    );
    let cleanup = daemon.terminate();
    assert_resources_released(&home, cleanup);
}

#[derive(Clone, Default)]
struct SubagentMockState {
    requests: Arc<Mutex<usize>>,
    retry_attempts: Arc<Mutex<HashMap<String, usize>>>,
    request_message_counts: Arc<Mutex<Vec<usize>>>,
}

fn mock_openai_text_sse(content: &str) -> String {
    let event = json!({
        "choices": [{"delta": {"content": content}, "finish_reason": "stop"}]
    });
    format!("data: {event}\n\ndata: [DONE]\n\n")
}

fn mock_openai_tool_sse(id: &str, name: &str, arguments: serde_json::Value) -> String {
    let arguments = serde_json::to_string(&arguments).expect("tool arguments should serialize");
    let event = json!({
        "choices": [{
            "delta": {"tool_calls": [{
                "index": 0,
                "id": id,
                "type": "function",
                "function": {"name": name, "arguments": arguments}
            }]},
            "finish_reason": "tool_calls"
        }]
    });
    format!("data: {event}\n\ndata: [DONE]\n\n")
}

fn mock_openai_delta_sse(content: &str) -> String {
    let event = json!({"choices": [{"delta": {"content": content}}]});
    format!("data: {event}\n\n")
}

fn long_running_openai_response() -> axum::response::Response {
    let stream = futures::stream::unfold(0usize, |index| async move {
        tokio::time::sleep(Duration::from_millis(180)).await;
        let chunk = match index {
            0..=8 => mock_openai_delta_sse("working "),
            9 => mock_openai_delta_sse("NATURAL-COMPLETION-SENTINEL"),
            10 => "data: [DONE]\n\n".to_string(),
            _ => return None,
        };
        Some((Ok::<String, Infallible>(chunk), index + 1))
    });
    axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .expect("long-running SSE response should build")
}

fn request_exposes_tool(request: &serde_json::Value, tool_name: &str) -> bool {
    request["tools"].as_array().is_some_and(|tools| {
        tools.iter().any(|tool| {
            tool["function"]["name"].as_str() == Some(tool_name)
                || tool["name"].as_str() == Some(tool_name)
        })
    })
}

async fn subagent_mock_handler(
    State(state): State<SubagentMockState>,
    Json(request): Json<serde_json::Value>,
) -> axum::response::Response {
    let request_number = {
        let mut requests = state.requests.lock().await;
        *requests += 1;
        *requests
    };
    state
        .request_message_counts
        .lock()
        .await
        .push(request["messages"].as_array().map_or(0, Vec::len));
    let is_parent = request_exposes_tool(&request, "task");
    let last = request["messages"]
        .as_array()
        .and_then(|messages| messages.last());
    let last_role = last
        .and_then(|message| message["role"].as_str())
        .unwrap_or_default();
    let last_content = last
        .and_then(|message| message["content"].as_str())
        .unwrap_or_default();
    if request
        .to_string()
        .contains("You compress older conversation context")
    {
        return Json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "The earlier repeated boundary turns were summarized without changing the current request."
                },
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1200, "completion_tokens": 20}
        }))
        .into_response();
    }
    if !is_parent && request.to_string().contains("partial failure sentinel") {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "intentional partial orchestration failure",
        )
            .into_response();
    }
    if is_parent && request.to_string().contains("long streaming stop") {
        return long_running_openai_response();
    }
    if is_parent
        && (last_content.contains("transient retry succeeds")
            || last_content.contains("terminal retry failure"))
    {
        let label = if last_content.contains("terminal retry failure") {
            "terminal"
        } else {
            "success"
        };
        let attempt = {
            let mut attempts = state.retry_attempts.lock().await;
            let attempt = attempts.entry(label.to_string()).or_default();
            *attempt += 1;
            *attempt
        };
        if label == "terminal" || attempt == 1 {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "TRANSIENT-LLM-ERROR-SENTINEL",
            )
                .into_response();
        }
        return (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            mock_openai_text_sse("Retry recovered without duplicating the provider error."),
        )
            .into_response();
    }
    let body = if is_parent && last_content.contains("persist terminal outcome") {
        mock_openai_text_sse("Persisted terminal outcome response.")
    } else if is_parent && last_content.contains("compression boundary history") {
        mock_openai_text_sse("Compression boundary history response.")
    } else if !is_parent {
        mock_openai_text_sse("Sub-agent inspected the isolated workspace successfully.")
    } else if last_role == "tool" {
        mock_openai_text_sse("Parent run received the delegated result and completed.")
    } else if last_content.contains("failing delegated task") {
        mock_openai_tool_sse(
            &format!("task-failure-{request_number}"),
            "task",
            json!({
                "agent": "explore",
                "prompt": "Trigger the partial failure sentinel without affecting the daemon."
            }),
        )
    } else if last_content.contains("partial orchestration") {
        mock_openai_tool_sse(
            &format!("orchestrate-{request_number}"),
            "orchestrate",
            json!({
                "tasks": [
                    {
                        "id": "inspect",
                        "agent": "explore",
                        "prompt": "Inspect the isolated workspace.",
                        "depends_on": []
                    },
                    {
                        "id": "missing",
                        "agent": "explore",
                        "prompt": "Trigger the partial failure sentinel without affecting the daemon.",
                        "depends_on": ["inspect"]
                    }
                ]
            }),
        )
    } else {
        mock_openai_tool_sse(
            &format!("task-{request_number}"),
            "task",
            json!({
                "agent": "explore",
                "prompt": "Inspect the isolated workspace and report success."
            }),
        )
    };
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

fn write_mock_provider_config_with_context(
    home: &Path,
    base_url: &str,
    context_window: u64,
    max_tokens: u64,
) {
    let config_dir = home.join(".lingclaw");
    std::fs::create_dir_all(&config_dir).expect("config directory should be created");
    let config = json!({
        "settings": {
            "maxLlmRetries": 0,
            "structuredMemory": false,
            "dailyReflection": false,
            "enableStateDigest": false,
            "enableTaskPlan": false,
            "enableGroups": false,
            "enableS3": false,
            "subAgentTimeout": 10
        },
        "models": {"providers": {"mock": {
            "baseUrl": base_url,
            "apiKey": "test-key",
            "api": "openai-completions",
            "models": [{
                "id": "mock-model",
                "name": "mock-model",
                "reasoning": false,
                "input": ["text"],
                "contextWindow": context_window,
                "maxTokens": max_tokens
            }]
        }}},
        "agents": {"defaults": {"model": {
            "primary": "mock/mock-model",
            "sub-agent": "mock/mock-model"
        }}}
    });
    std::fs::write(
        config_dir.join(".lingclaw.json"),
        serde_json::to_vec_pretty(&config).expect("config should serialize"),
    )
    .expect("mock provider config should be written");
}

fn write_mock_provider_config(home: &Path, base_url: &str) {
    write_mock_provider_config_with_context(home, base_url, 32_000, 2_048);
}

fn write_memory_mock_provider_config(home: &Path, base_url: &str) {
    write_mock_provider_config(home, base_url);
    let path = home.join(".lingclaw").join(".lingclaw.json");
    let mut config: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&path).expect("mock Provider config should be readable"),
    )
    .expect("mock Provider config should parse");
    config["settings"]["structuredMemory"] = json!(true);
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&config).expect("memory config should serialize"),
    )
    .expect("memory Provider config should be written");
}

#[derive(Clone, Default)]
struct AuxiliaryShutdownMockState {
    memory_arrived: Arc<Notify>,
    release_memory: Arc<Notify>,
}

async fn auxiliary_shutdown_mock_handler(
    State(state): State<AuxiliaryShutdownMockState>,
    Json(request): Json<serde_json::Value>,
) -> axum::response::Response {
    if request["stream"].as_bool() == Some(true) {
        return (
            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
            mock_openai_text_sse("Primary response completed."),
        )
            .into_response();
    }

    state.memory_arrived.notify_one();
    state.release_memory.notified().await;
    Json(json!({
        "choices": [{"message": {"content": "{\"update_facts\":[],\"delete_facts\":[]}"}}],
        "usage": {"prompt_tokens": 7, "completion_tokens": 3}
    }))
    .into_response()
}

async fn receive_run_events<S>(
    websocket: &mut tokio_tungstenite::WebSocketStream<S>,
) -> Vec<serde_json::Value>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut events = Vec::new();
    loop {
        let message = tokio::time::timeout_at(deadline, websocket.next())
            .await
            .expect("run should report a terminal event before its deadline")
            .expect("WebSocket should remain connected")
            .expect("WebSocket frame should decode");
        match message {
            Message::Text(text) => {
                let event: serde_json::Value =
                    serde_json::from_str(&text).expect("WebSocket event should be JSON");
                let terminal = event["type"] == "done"
                    || (event["type"] == "error" && event["run_terminal"] == true);
                events.push(event);
                if terminal {
                    return events;
                }
            }
            Message::Ping(payload) => {
                websocket
                    .send(Message::Pong(payload))
                    .await
                    .expect("pong should send");
            }
            Message::Close(frame) => panic!("daemon closed the WebSocket early: {frame:?}"),
            _ => {}
        }
    }
}

async fn receive_history<S>(
    websocket: &mut tokio_tungstenite::WebSocketStream<S>,
) -> serde_json::Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), websocket.next())
            .await
            .expect("Session history should arrive before its deadline")
            .expect("WebSocket should remain connected")
            .expect("WebSocket frame should decode");
        match message {
            Message::Text(text) => {
                let event: serde_json::Value =
                    serde_json::from_str(&text).expect("WebSocket event should be JSON");
                if event["type"] == "history" {
                    return event;
                }
            }
            Message::Ping(payload) => {
                websocket
                    .send(Message::Pong(payload))
                    .await
                    .expect("pong should send");
            }
            Message::Close(frame) => panic!("daemon closed the WebSocket early: {frame:?}"),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graceful_daemon_shutdown_cancels_and_drains_blocked_memory_provider() {
    let mock_state = AuxiliaryShutdownMockState::default();
    let mock_app = Router::new()
        .route("/chat/completions", post(auxiliary_shutdown_mock_handler))
        .with_state(mock_state.clone());
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock Provider should bind");
    let mock_address = mock_listener.local_addr().expect("mock Provider address");
    let mock_task = tokio::spawn(async move {
        let _ = axum::serve(mock_listener, mock_app).await;
    });

    let mut daemon = spawn_lingclaw_prepared(
        "auxiliary-shutdown",
        &["--serve"],
        Stdio::null(),
        |home, _| {
            write_memory_mock_provider_config(home, &format!("http://{mock_address}"));
        },
    );
    let home = daemon.home.clone();
    let port = daemon.port;
    let health_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if lingclaw_health(port, health_deadline).is_ok() {
            break;
        }
        assert!(
            daemon.child.try_wait().expect("daemon status").is_none(),
            "daemon exited before becoming healthy"
        );
        assert!(Instant::now() < health_deadline, "daemon health timed out");
        thread::sleep(Duration::from_millis(50));
    }

    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .expect("Session WebSocket should connect");
    let _ = receive_history(&mut websocket).await;
    websocket
        .send(Message::Text(
            json!({"text": "complete a run and queue memory", "plan_mode": false})
                .to_string()
                .into(),
        ))
        .await
        .expect("prompt should send");
    let events = receive_run_events(&mut websocket).await;
    assert_eq!(
        events.last().and_then(|event| event["type"].as_str()),
        Some("done")
    );
    tokio::time::timeout(Duration::from_secs(8), mock_state.memory_arrived.notified())
        .await
        .expect("structured Memory Provider request should start");
    websocket
        .close(None)
        .await
        .expect("WebSocket should close before shutdown");

    let shutdown_token = std::fs::read_to_string(
        home.join(".lingclaw")
            .join(format!("shutdown-{port}.token")),
    )
    .expect("shutdown token should be readable");
    let response = reqwest::Client::new()
        .post(format!("http://127.0.0.1:{port}/api/shutdown"))
        .bearer_auth(shutdown_token.trim())
        .send()
        .await
        .expect("shutdown request should send");
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let started = Instant::now();
    let output = daemon
        .wait_for_output(Duration::from_secs(6))
        .unwrap_or_else(|timeout| panic!("{} did not exit: {timeout:?}", timeout.description));
    let elapsed = started.elapsed();
    mock_state.release_memory.notify_waiters();
    mock_task.abort();

    assert!(output.status.success());
    assert!(
        elapsed < Duration::from_secs(5),
        "graceful shutdown waited for the blocked Provider timeout: {elapsed:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "daemon stderr: {stderr}");
    assert_resources_released(&home, output.cleanup);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_server_survives_repeated_explore_tasks_and_partial_orchestration() {
    let mock_state = SubagentMockState::default();
    let mock_app = Router::new()
        .route("/chat/completions", post(subagent_mock_handler))
        .with_state(mock_state.clone());
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock provider should bind");
    let mock_address = mock_listener.local_addr().expect("mock provider address");
    let mock_task = tokio::spawn(async move {
        let _ = axum::serve(mock_listener, mock_app).await;
    });

    let mut daemon =
        spawn_lingclaw_prepared("subagent-stack", &["--serve"], Stdio::null(), |home, _| {
            write_mock_provider_config(home, &format!("http://{mock_address}"))
        });
    let home = daemon.home.clone();
    let port = daemon.port;
    let health_deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if lingclaw_health(port, health_deadline).is_ok() {
            break;
        }
        assert!(
            daemon.child.try_wait().expect("daemon status").is_none(),
            "daemon exited before becoming healthy"
        );
        assert!(Instant::now() < health_deadline, "daemon health timed out");
        thread::sleep(Duration::from_millis(50));
    }

    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .expect("Session WebSocket should connect");
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), websocket.next())
            .await
            .expect("initial Session payload should arrive")
            .expect("initial WebSocket should stay open")
            .expect("initial frame should decode");
        if let Message::Text(text) = message {
            let event: serde_json::Value = serde_json::from_str(&text).expect("initial event JSON");
            if event["type"] == "history" {
                break;
            }
        }
    }

    for iteration in 0..10 {
        websocket
            .send(Message::Text(
                json!({"text": format!("delegated explore run {iteration}"), "plan_mode": false})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("prompt should send");
        let events = receive_run_events(&mut websocket).await;
        assert!(events.iter().any(|event| event["type"] == "task_started"));
        assert!(events.iter().any(|event| event["type"] == "task_completed"));
        assert_eq!(
            events.last().and_then(|event| event["type"].as_str()),
            Some("done")
        );
        assert!(
            daemon.child.try_wait().expect("daemon status").is_none(),
            "daemon exited during repeated delegated run {iteration}"
        );
    }

    websocket
        .send(Message::Text(
            json!({"text": "run a failing delegated task", "plan_mode": false})
                .to_string()
                .into(),
        ))
        .await
        .expect("failing delegated prompt should send");
    let events = receive_run_events(&mut websocket).await;
    assert!(events.iter().any(|event| event["type"] == "task_started"));
    assert!(events.iter().any(|event| event["type"] == "task_failed"));
    assert_eq!(
        events.last().and_then(|event| event["type"].as_str()),
        Some("done")
    );
    assert!(
        daemon.child.try_wait().expect("daemon status").is_none(),
        "daemon exited after a delegated failure"
    );

    websocket
        .send(Message::Text(
            json!({"text": "run partial orchestration", "plan_mode": false})
                .to_string()
                .into(),
        ))
        .await
        .expect("orchestration prompt should send");
    let events = receive_run_events(&mut websocket).await;
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "orchestrate_started"),
        "orchestration events: {events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "orchestrate_task_failed"),
        "orchestration events: {events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|event| event["type"] == "orchestrate_completed"),
        "orchestration events: {events:#?}"
    );
    assert_eq!(
        events.last().and_then(|event| event["type"].as_str()),
        Some("done")
    );
    assert_eq!(events.last().unwrap()["phase"], "partial");
    assert!(lingclaw_health(port, Instant::now() + Duration::from_secs(2)).is_ok());

    websocket.close(None).await.expect("WebSocket should close");
    let output = daemon.terminate_and_capture();
    mock_task.abort();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("overflowed its stack"),
        "daemon stderr: {stderr}"
    );
    assert_resources_released(&home, output.cleanup);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_interrupts_a_live_provider_stream_without_natural_completion() {
    let mock_app = Router::new()
        .route("/chat/completions", post(subagent_mock_handler))
        .with_state(SubagentMockState::default());
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock provider should bind");
    let mock_address = mock_listener.local_addr().expect("mock provider address");
    let mock_task = tokio::spawn(async move {
        let _ = axum::serve(mock_listener, mock_app).await;
    });

    let mut daemon =
        spawn_lingclaw_prepared("stop-stream", &["--serve"], Stdio::null(), |home, _| {
            write_mock_provider_config(home, &format!("http://{mock_address}"))
        });
    let home = daemon.home.clone();
    let port = daemon.port;
    let health_deadline = Instant::now() + Duration::from_secs(15);
    while lingclaw_health(port, health_deadline).is_err() {
        assert!(
            daemon.child.try_wait().expect("daemon status").is_none(),
            "daemon exited before becoming healthy"
        );
        assert!(Instant::now() < health_deadline, "daemon health timed out");
        thread::sleep(Duration::from_millis(50));
    }

    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .expect("Session WebSocket should connect");
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), websocket.next())
            .await
            .expect("initial Session payload should arrive")
            .expect("initial WebSocket should stay open")
            .expect("initial frame should decode");
        if let Message::Text(text) = message {
            let event: serde_json::Value = serde_json::from_str(&text).expect("initial event JSON");
            if event["type"] == "history" {
                break;
            }
        }
    }

    websocket
        .send(Message::Text(
            json!({"text": "long streaming stop", "plan_mode": false})
                .to_string()
                .into(),
        ))
        .await
        .expect("long-running prompt should send");
    let mut before_stop = Vec::new();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), websocket.next())
            .await
            .expect("stream should start")
            .expect("WebSocket should remain connected")
            .expect("WebSocket frame should decode");
        if let Message::Text(text) = message {
            let event: serde_json::Value = serde_json::from_str(&text).expect("stream event JSON");
            let saw_delta = event["type"] == "delta";
            before_stop.push(event);
            if saw_delta {
                break;
            }
        }
    }
    tokio::time::sleep(Duration::from_millis(470)).await;
    let stop_started = Instant::now();
    websocket
        .send(Message::Text("/stop".into()))
        .await
        .expect("stop command should send");
    let after_stop = receive_run_events(&mut websocket).await;
    assert!(
        stop_started.elapsed() < Duration::from_secs(1),
        "stop should close the active stream within one second"
    );
    let terminal: Vec<_> = after_stop
        .iter()
        .filter(|event| {
            event["type"] == "done" || (event["type"] == "error" && event["run_terminal"] == true)
        })
        .collect();
    assert_eq!(terminal.len(), 1, "terminal events: {after_stop:#?}");
    assert_eq!(terminal[0]["type"], "done");
    assert_eq!(terminal[0]["phase"], "stopped");
    assert_eq!(terminal[0]["reason"], "user_stop");
    let all_events = before_stop.iter().chain(after_stop.iter());
    assert!(
        !all_events.into_iter().any(|event| {
            event["content"]
                .as_str()
                .is_some_and(|content| content.contains("NATURAL-COMPLETION-SENTINEL"))
        }),
        "natural completion content must be discarded after stop"
    );
    assert!(lingclaw_health(port, Instant::now() + Duration::from_secs(2)).is_ok());

    websocket.close(None).await.expect("WebSocket should close");
    let output = daemon.terminate_and_capture();
    mock_task.abort();
    assert_resources_released(&home, output.cleanup);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn llm_retry_is_transient_progress_and_terminal_failure_is_not_duplicated() {
    let mock_app = Router::new()
        .route("/chat/completions", post(subagent_mock_handler))
        .with_state(SubagentMockState::default());
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock provider should bind");
    let mock_address = mock_listener.local_addr().expect("mock provider address");
    let mock_task = tokio::spawn(async move {
        let _ = axum::serve(mock_listener, mock_app).await;
    });
    let mut daemon =
        spawn_lingclaw_prepared("llm-retry", &["--serve"], Stdio::null(), |home, _| {
            write_mock_provider_config(home, &format!("http://{mock_address}"))
        });
    let home = daemon.home.clone();
    let port = daemon.port;
    let health_deadline = Instant::now() + Duration::from_secs(15);
    while lingclaw_health(port, health_deadline).is_err() {
        assert!(
            daemon.child.try_wait().expect("daemon status").is_none(),
            "daemon exited before becoming healthy"
        );
        assert!(Instant::now() < health_deadline, "daemon health timed out");
        thread::sleep(Duration::from_millis(50));
    }
    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .expect("Session WebSocket should connect");
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), websocket.next())
            .await
            .expect("initial Session payload should arrive")
            .expect("WebSocket should remain connected")
            .expect("initial frame should decode");
        if let Message::Text(text) = message {
            let event: serde_json::Value = serde_json::from_str(&text).expect("initial event JSON");
            if event["type"] == "history" {
                break;
            }
        }
    }

    websocket
        .send(Message::Text(
            json!({"text": "transient retry succeeds", "plan_mode": false})
                .to_string()
                .into(),
        ))
        .await
        .expect("retry-success prompt should send");
    let success_events = receive_run_events(&mut websocket).await;
    assert_eq!(
        success_events
            .iter()
            .filter(|event| event["type"] == "progress" && event["kind"] == "llm_retry")
            .count(),
        1
    );
    assert!(!success_events.iter().any(|event| {
        event["type"] == "system"
            && event["content"]
                .as_str()
                .is_some_and(|content| content.contains("TRANSIENT-LLM-ERROR-SENTINEL"))
    }));
    assert!(success_events.iter().any(|event| {
        event["type"] == "delta"
            && event["content"]
                .as_str()
                .is_some_and(|content| content.contains("Retry recovered"))
    }));
    assert_eq!(success_events.last().unwrap()["type"], "done");

    websocket
        .send(Message::Text(
            json!({"text": "terminal retry failure", "plan_mode": false})
                .to_string()
                .into(),
        ))
        .await
        .expect("retry-failure prompt should send");
    let failure_events = receive_run_events(&mut websocket).await;
    assert_eq!(
        failure_events
            .iter()
            .filter(|event| event["type"] == "progress" && event["kind"] == "llm_retry")
            .count(),
        1
    );
    assert_eq!(
        failure_events
            .iter()
            .filter(|event| event["type"] == "error" && event["run_terminal"] == true)
            .count(),
        1
    );
    assert!(!failure_events.iter().any(|event| event["type"] == "system"));
    let failed_run_id = failure_events
        .iter()
        .find(|event| event["type"] == "error" && event["run_terminal"] == true)
        .and_then(|event| event["run_id"].as_str())
        .expect("terminal failure should carry a stable run id")
        .to_string();
    assert!(lingclaw_health(port, Instant::now() + Duration::from_secs(2)).is_ok());

    websocket.close(None).await.expect("WebSocket should close");
    let (mut replay, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .expect("replay WebSocket should connect");
    let history = loop {
        let message = tokio::time::timeout(Duration::from_secs(5), replay.next())
            .await
            .expect("failure history should arrive")
            .expect("replay WebSocket should remain open")
            .expect("replay frame should decode");
        if let Message::Text(text) = message {
            let event: serde_json::Value =
                serde_json::from_str(&text).expect("history event should be JSON");
            if event["type"] == "history" {
                break event;
            }
        }
    };
    let persisted_failure = history["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .flat_map(|message| message["run_outcomes"].as_array().into_iter().flatten())
                .find(|outcome| outcome["run_id"].as_str() == Some(failed_run_id.as_str()))
        })
        .expect("terminal failure should be attached to history before reconnect");
    assert_eq!(persisted_failure["status"], "failed");
    replay
        .close(None)
        .await
        .expect("replay socket should close");
    let output = daemon.terminate_and_capture();
    mock_task.abort();
    assert_resources_released(&home, output.cleanup);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_run_outcome_survives_a_daemon_process_restart() {
    let mock_app = Router::new()
        .route("/chat/completions", post(subagent_mock_handler))
        .with_state(SubagentMockState::default());
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock provider should bind");
    let mock_address = mock_listener.local_addr().expect("mock provider address");
    let mock_task = tokio::spawn(async move {
        let _ = axum::serve(mock_listener, mock_app).await;
    });

    let mut first = spawn_lingclaw_prepared(
        "terminal-restart",
        &["--serve"],
        Stdio::null(),
        |home, _| write_mock_provider_config(home, &format!("http://{mock_address}")),
    );
    let home = first.home.clone();
    let port = first.port;
    let health_deadline = Instant::now() + Duration::from_secs(15);
    while lingclaw_health(port, health_deadline).is_err() {
        assert!(
            first.child.try_wait().expect("daemon status").is_none(),
            "first daemon exited before becoming healthy"
        );
        assert!(Instant::now() < health_deadline, "daemon health timed out");
        thread::sleep(Duration::from_millis(50));
    }
    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .expect("first Session WebSocket should connect");
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), websocket.next())
            .await
            .expect("initial history should arrive")
            .expect("first WebSocket should remain open")
            .expect("initial frame should decode");
        if let Message::Text(text) = message {
            let event: serde_json::Value = serde_json::from_str(&text).expect("history event JSON");
            if event["type"] == "history" {
                break;
            }
        }
    }
    websocket
        .send(Message::Text(
            json!({"text": "persist terminal outcome", "plan_mode": false})
                .to_string()
                .into(),
        ))
        .await
        .expect("prompt should send");
    let events = receive_run_events(&mut websocket).await;
    let done = events.last().expect("run should have a terminal event");
    assert_eq!(done["type"], "done");
    assert_eq!(done["phase"], "finish");
    assert_eq!(done["reason"], "complete");
    let run_id = done["run_id"]
        .as_str()
        .expect("done should carry a stable run id")
        .to_string();
    assert!(done["duration_ms"].as_u64().is_some());
    websocket
        .close(None)
        .await
        .expect("first WebSocket should close");
    first.stop_preserving_home();
    drop(first);

    let mut restarted = spawn_lingclaw_in_home(home.clone(), port, &["--serve"], Stdio::null());
    let restart_deadline = Instant::now() + Duration::from_secs(15);
    while lingclaw_health(port, restart_deadline).is_err() {
        assert!(
            restarted
                .child
                .try_wait()
                .expect("restarted daemon status")
                .is_none(),
            "restarted daemon exited before becoming healthy"
        );
        assert!(
            Instant::now() < restart_deadline,
            "restarted daemon health timed out"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .expect("restarted Session WebSocket should connect");
    let history = loop {
        let message = tokio::time::timeout(Duration::from_secs(5), websocket.next())
            .await
            .expect("restarted history should arrive")
            .expect("restarted WebSocket should remain open")
            .expect("restarted frame should decode");
        if let Message::Text(text) = message {
            let event: serde_json::Value =
                serde_json::from_str(&text).expect("restarted history event JSON");
            if event["type"] == "history" {
                break event;
            }
        }
    };
    let outcomes: Vec<&serde_json::Value> = history["messages"]
        .as_array()
        .expect("history messages should be an array")
        .iter()
        .flat_map(|message| message["run_outcomes"].as_array().into_iter().flatten())
        .collect();
    let outcome = outcomes
        .iter()
        .find(|outcome| outcome["run_id"] == run_id)
        .expect("restarted history should include the exact terminal run outcome");
    assert_eq!(outcome["status"], "completed");
    assert_eq!(outcome["phase"], "finish");
    assert_eq!(outcome["reason"], "complete");
    assert!(outcome["duration_ms"].as_u64().is_some());
    assert!(outcome["start_message_index"].as_u64().is_some());
    assert!(outcome["end_message_index"].as_u64().is_some());

    websocket
        .close(None)
        .await
        .expect("restarted WebSocket should close");
    let output = restarted.terminate_and_capture();
    mock_task.abort();
    assert_resources_released(&home, output.cleanup);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_boundaries_survive_real_auto_compression_for_completion_and_stop() {
    let mock_state = SubagentMockState::default();
    let mock_app = Router::new()
        .route("/chat/completions", post(subagent_mock_handler))
        .with_state(mock_state.clone());
    let mock_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("mock provider should bind");
    let mock_address = mock_listener.local_addr().expect("mock provider address");
    let mock_task = tokio::spawn(async move {
        let _ = axum::serve(mock_listener, mock_app).await;
    });

    let mut daemon = spawn_lingclaw_prepared(
        "terminal-compression-boundary",
        &["--serve"],
        Stdio::null(),
        |home, _| {
            write_mock_provider_config_with_context(
                home,
                &format!("http://{mock_address}"),
                24_000,
                512,
            )
        },
    );
    let home = daemon.home.clone();
    let port = daemon.port;
    let health_deadline = Instant::now() + Duration::from_secs(15);
    while lingclaw_health(port, health_deadline).is_err() {
        assert!(
            daemon.child.try_wait().expect("daemon status").is_none(),
            "daemon exited before becoming healthy"
        );
        assert!(Instant::now() < health_deadline, "daemon health timed out");
        thread::sleep(Duration::from_millis(50));
    }

    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .expect("Session WebSocket should connect");
    let _ = receive_history(&mut websocket).await;

    let mut completed_run_id = None;
    let mut observed_compression_events = Vec::new();
    let repeated_prefix = "compression boundary history repeated user";
    for iteration in 0..16 {
        let prompt = format!("{repeated_prefix} {iteration} {}", "A".repeat(6_500));
        websocket
            .send(Message::Text(
                json!({"text": prompt, "plan_mode": false})
                    .to_string()
                    .into(),
            ))
            .await
            .expect("history-building prompt should send");
        let events = receive_run_events(&mut websocket).await;
        observed_compression_events.extend(
            events
                .iter()
                .filter(|event| {
                    event["type"]
                        .as_str()
                        .is_some_and(|kind| kind.starts_with("context_compress"))
                })
                .cloned(),
        );
        let terminal = events.last().expect("run should report a terminal event");
        assert_eq!(terminal["type"], "done", "events: {events:#?}");
        if events
            .iter()
            .any(|event| event["type"] == "context_compressed")
        {
            completed_run_id = terminal["run_id"].as_str().map(str::to_string);
            break;
        }
    }
    let provider_requests = *mock_state.requests.lock().await;
    let request_message_counts = mock_state.request_message_counts.lock().await.clone();
    let completed_run_id = completed_run_id.unwrap_or_else(|| {
        panic!(
            "a production BeforeAnalyze compression should run; provider requests={}, message_counts={request_message_counts:?}, events={observed_compression_events:#?}",
            provider_requests,
        )
    });

    websocket
        .close(None)
        .await
        .expect("first WebSocket should close");
    daemon.stop_preserving_home();
    drop(daemon);

    let mut restarted = spawn_lingclaw_in_home(home.clone(), port, &["--serve"], Stdio::null());
    let restart_deadline = Instant::now() + Duration::from_secs(15);
    while lingclaw_health(port, restart_deadline).is_err() {
        assert!(
            restarted
                .child
                .try_wait()
                .expect("restarted daemon status")
                .is_none(),
            "restarted daemon exited before becoming healthy"
        );
        assert!(
            Instant::now() < restart_deadline,
            "restarted daemon health timed out"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .expect("restarted Session WebSocket should connect");
    let history = receive_history(&mut websocket).await;
    let completed_outcome = history["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|message| message["run_outcomes"].as_array().into_iter().flatten())
        .find(|outcome| outcome["run_id"].as_str() == Some(completed_run_id.as_str()))
        .expect("completed compressed run should survive restart");
    assert_eq!(completed_outcome["status"], "completed");
    let completed_start = completed_outcome["start_message_index"]
        .as_u64()
        .expect("completed outcome should have a start boundary");
    assert!(history["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["message_index"].as_u64() == Some(completed_start)
                && message["role"] == "user"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.starts_with(repeated_prefix))
        })
    }));

    // The auto-generated summary itself occupies one retained turn. Let one
    // ordinary run advance the window so the following stopped run compresses
    // a full historical user/assistant turn rather than a summary-only slice.
    websocket
        .send(Message::Text(
            json!({
                "text": format!(
                    "compression boundary history bridge {}",
                    "B".repeat(6_500)
                ),
                "plan_mode": false
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("compression bridge prompt should send");
    let bridge_events = receive_run_events(&mut websocket).await;
    assert_eq!(
        bridge_events
            .last()
            .and_then(|event| event["type"].as_str()),
        Some("done")
    );

    let stop_prompt = format!(
        "long streaming stop compression boundary history {}",
        "Z".repeat(6_500)
    );
    websocket
        .send(Message::Text(
            json!({"text": stop_prompt, "plan_mode": false})
                .to_string()
                .into(),
        ))
        .await
        .expect("stopped compressed prompt should send");
    let mut before_stop = Vec::new();
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), websocket.next())
            .await
            .expect("compressed stream should start")
            .expect("WebSocket should remain connected")
            .expect("WebSocket frame should decode");
        if let Message::Text(text) = message {
            let event: serde_json::Value =
                serde_json::from_str(&text).expect("stream event should be JSON");
            let saw_delta = event["type"] == "delta";
            before_stop.push(event);
            if saw_delta {
                break;
            }
        }
    }
    assert!(
        before_stop
            .iter()
            .any(|event| event["type"] == "context_compressed"),
        "the stopped run should exercise the real compression hook: {before_stop:#?}"
    );
    websocket
        .send(Message::Text("/stop".into()))
        .await
        .expect("stop command should send");
    let stopped_events = receive_run_events(&mut websocket).await;
    let stopped = stopped_events
        .last()
        .expect("stopped run should report a terminal event");
    assert_eq!(stopped["type"], "done");
    assert_eq!(stopped["phase"], "stopped");
    assert_eq!(stopped["reason"], "user_stop");
    let stopped_run_id = stopped["run_id"]
        .as_str()
        .expect("stopped outcome should carry a run id")
        .to_string();

    websocket
        .close(None)
        .await
        .expect("second WebSocket should close");
    restarted.stop_preserving_home();
    drop(restarted);

    let mut final_daemon = spawn_lingclaw_in_home(home.clone(), port, &["--serve"], Stdio::null());
    let final_deadline = Instant::now() + Duration::from_secs(15);
    while lingclaw_health(port, final_deadline).is_err() {
        assert!(
            final_daemon
                .child
                .try_wait()
                .expect("final daemon status")
                .is_none(),
            "final daemon exited before becoming healthy"
        );
        assert!(Instant::now() < final_deadline, "final health timed out");
        thread::sleep(Duration::from_millis(50));
    }
    let (mut websocket, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .expect("final Session WebSocket should connect");
    let history = receive_history(&mut websocket).await;
    let stopped_outcome = history["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|message| message["run_outcomes"].as_array().into_iter().flatten())
        .find(|outcome| outcome["run_id"].as_str() == Some(stopped_run_id.as_str()))
        .expect("stopped compressed run should survive restart");
    assert_eq!(stopped_outcome["status"], "stopped");
    let stopped_start = stopped_outcome["start_message_index"]
        .as_u64()
        .expect("stopped outcome should have a start boundary");
    assert!(history["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["message_index"].as_u64() == Some(stopped_start)
                && message["role"] == "user"
                && message["content"]
                    .as_str()
                    .is_some_and(|content| content.starts_with("long streaming stop"))
        })
    }));

    websocket.close(None).await.expect("WebSocket should close");
    let output = final_daemon.terminate_and_capture();
    mock_task.abort();
    assert_resources_released(&home, output.cleanup);
}

// Production daemon regressions for the cross-round terminal/Plan lifecycle.
async fn round21_mock_handler(
    State(state): State<SubagentMockState>,
    Json(request): Json<serde_json::Value>,
) -> axum::response::Response {
    let number = {
        let mut n = state.requests.lock().await;
        *n += 1;
        *n
    };
    let messages = request["messages"].as_array().expect("provider messages");
    let user = messages
        .iter()
        .rposition(|m| m["role"] == "user")
        .unwrap_or(0);
    let prompt = messages[user]["content"].as_str().unwrap_or_default();
    let tools = messages[user + 1..]
        .iter()
        .filter(|m| m["role"] == "tool")
        .count();
    if prompt.contains("R21_FAIL") {
        return (StatusCode::INTERNAL_SERVER_ERROR, "R21 controlled failure").into_response();
    }
    if prompt.contains("R21_STOP") {
        return long_running_openai_response();
    }
    let body = if request_exposes_tool(&request, "update_plan") {
        mock_openai_tool_sse(
            &format!("r21-{number}"),
            "update_plan",
            json!({"base_revision":1,"updates":[{"id":"inspect","status":"in_progress","note":"Hard cap fixture"}]}),
        )
    } else if request_exposes_tool(&request, "submit_plan") {
        if prompt.contains("R21_HARD") {
            mock_openai_tool_sse(
                &format!("r21-{number}"),
                "read_file",
                json!({"path":"README.md"}),
            )
        } else {
            let waiting = prompt.contains("R21_WAIT");
            let mut plan = json!({"state":if waiting {"needs_input"} else {"ready"},"title":"R21 formal plan","goal":"Inspect the isolated fixture","steps":[{"id":"inspect","title":"Inspect fixture"}]});
            if waiting {
                plan["questions"] = json!([{"id":"scope","prompt":"Choose a scope.","options":[{"id":"small","label":"Small"},{"id":"all","label":"All"}]}]);
            } else {
                plan["acceptance_criteria"] = json!(["The fixture exists."]);
                plan["completion_checks"] = json!([{"id":"fixture","step_id":"inspect","covers":[{"section":"acceptance_criteria","index":0}],"kind":"workspace_path","path":"README.md","expected_path_type":"file"}]);
            }
            mock_openai_tool_sse(&format!("r21-{number}"), "submit_plan", plan)
        }
    } else if prompt.contains("R21_RETRY") || prompt.contains("R21_OTHER") {
        if tools < 2 {
            let args = if tools == 0 && prompt.contains("R21_RETRY") {
                json!({"path":"README.md","start_line":10,"end_line":1})
            } else {
                json!({"path":if tools == 0 {"missing.txt"} else {"README.md"}})
            };
            mock_openai_tool_sse(&format!("r21-{number}"), "read_file", args)
        } else {
            mock_openai_text_sse("The requested tool checks ended.")
        }
    } else if tools == 0 {
        mock_openai_tool_sse(
            &format!("r21-{number}"),
            "read_file",
            json!({"path":"README.md"}),
        )
    } else {
        mock_openai_text_sse("The fixture was read.")
    };
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        body,
    )
        .into_response()
}

fn round21_outcomes(home: &Path) -> Vec<serde_json::Value> {
    let db = rusqlite::Connection::open_with_flags(
        home.join(".lingclaw/lingclaw.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut stmt = db.prepare("SELECT run_id,status,phase,start_message_index,end_message_index FROM session_run_outcomes WHERE session_id='main' ORDER BY end_message_index,finished_at,run_id").unwrap();
    stmt.query_map([], |row| Ok(json!({"run_id":row.get::<_,String>(0)?,"status":row.get::<_,String>(1)?,"phase":row.get::<_,String>(2)?,"start":row.get::<_,i64>(3)?,"end":row.get::<_,i64>(4)?}))).unwrap().collect::<Result<Vec<_>,_>>().unwrap()
}

async fn round21_wait_event<S>(
    ws: &mut tokio_tungstenite::WebSocketStream<S>,
    kind: &str,
) -> serde_json::Value
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let frame = tokio::time::timeout_at(deadline, ws.next())
            .await
            .expect("event deadline")
            .expect("socket open")
            .expect("frame");
        if let Message::Text(text) = frame {
            let event: serde_json::Value = serde_json::from_str(&text).unwrap();
            if event["type"] == kind {
                return event;
            }
            assert_ne!(event["type"], "error", "unexpected error: {event}");
        }
    }
}

async fn round21_start_daemon(label: &str) -> (ChildGuard, tokio::task::JoinHandle<()>) {
    let mock = Router::new()
        .route("/chat/completions", post(round21_mock_handler))
        .with_state(SubagentMockState::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, mock).await;
    });
    let mut daemon = spawn_lingclaw_prepared(label, &["--serve"], Stdio::null(), |home, _| {
        write_mock_provider_config_with_context(
            home,
            &format!("http://{address}"),
            1_000_000,
            2048,
        );
        let workspace = home.join(".lingclaw/main/workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("README.md"), "R21 isolated fixture\n").unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    while lingclaw_health(daemon.port, deadline).is_err() {
        assert!(daemon.child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    (daemon, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn round21_daemon_preserves_mixed_runs_across_switch_reconnect_and_restart() {
    let (mut daemon, mock) = round21_start_daemon("round21-mixed").await;
    let home = daemon.home.clone();
    let port = daemon.port;
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .unwrap();
    receive_history(&mut ws).await;
    let mut expected = Vec::new();
    let mut first_system_time = None;
    for (text, status, plan_mode) in [
        ("R21_SUCCESS first goal", "completed", false),
        ("R21_FAIL next goal", "failed", false),
        ("R21_OTHER different file", "partial", false),
        ("R21_RETRY same file", "completed", false),
        ("R21_WAIT choose scope", "waiting_user", true),
    ] {
        if expected.len() == 1 {
            let seconds = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let delay = 61 - seconds % 60;
            eprintln!("R21 waiting {delay}s to exercise the daemon's real system-time refresh");
            tokio::time::sleep(Duration::from_secs(delay)).await;
        }
        ws.send(Message::Text(
            json!({"text":text,"plan_mode":plan_mode})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
        let events = receive_run_events(&mut ws).await;
        let terminal = events.last().unwrap();
        let outcomes = round21_outcomes(&home);
        if expected.len() < 2 {
            let db = rusqlite::Connection::open_with_flags(
                home.join(".lingclaw/lingclaw.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let system: String = db
                .query_row(
                    "SELECT content FROM session_messages WHERE session_id='main' AND position=0",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let current = system
                .lines()
                .find(|line| line.contains("Current system local time:"))
                .expect("real clock prompt line")
                .to_string();
            if let Some(previous) = first_system_time.as_ref() {
                assert_ne!(&current, previous);
                eprintln!("R21 real system time changed: {previous} -> {current}");
            }
            first_system_time = Some(current);
        }
        assert_eq!(
            outcomes.len(),
            expected.len() + 1,
            "all preceding facts survive {text}"
        );
        let current = outcomes
            .iter()
            .find(|o| o["run_id"] == terminal["run_id"])
            .unwrap();
        assert_eq!(current["status"], status, "events={events:#?}");
        for old in &expected {
            assert!(outcomes.contains(old));
        }
        expected.push(current.clone());
        if plan_mode {
            let plan = events
                .iter()
                .find(|e| e["type"] == "plan_state" && e["plan"]["status"] == "needs_input")
                .unwrap()["plan"]
                .clone();
            ws.send(Message::Text(json!({"plan_action":{"action":"discard","plan_id":plan["plan_id"],"revision":plan["revision"]}}).to_string().into())).await.unwrap();
            assert_eq!(
                round21_wait_event(&mut ws, "plan_state").await["plan"]["status"],
                "discarded"
            );
        }
    }
    ws.send(Message::Text(
        json!({"text":"R21_STOP last goal"}).to_string().into(),
    ))
    .await
    .unwrap();
    round21_wait_event(&mut ws, "start").await;
    ws.send(Message::Text("/stop".into())).await.unwrap();
    let terminal = receive_run_events(&mut ws).await;
    let all = round21_outcomes(&home);
    let stopped = all
        .iter()
        .find(|o| o["run_id"] == terminal.last().unwrap()["run_id"])
        .unwrap();
    assert_eq!(stopped["status"], "stopped");
    expected.push(stopped.clone());
    // Switching away and back reuses the real socket's history protocol.
    ws.send(Message::Text("/switch round21-other".into()))
        .await
        .unwrap();
    receive_history(&mut ws).await;
    ws.send(Message::Text("/switch main".into())).await.unwrap();
    let history = receive_history(&mut ws).await;
    let facts = history["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|m| m["run_outcomes"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    for old in &expected {
        assert!(
            facts
                .iter()
                .any(|o| o["run_id"] == old["run_id"] && o["status"] == old["status"])
        );
    }
    ws.close(None).await.unwrap();
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .unwrap();
    receive_history(&mut ws).await;
    assert_eq!(round21_outcomes(&home), expected);
    ws.close(None).await.unwrap();
    daemon.stop_preserving_home();
    drop(daemon);
    let mut restarted = spawn_lingclaw_in_home(home.clone(), port, &["--serve"], Stdio::null());
    let deadline = Instant::now() + Duration::from_secs(15);
    while lingclaw_health(port, deadline).is_err() {
        assert!(restarted.child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .unwrap();
    receive_history(&mut ws).await;
    assert_eq!(round21_outcomes(&home), expected);
    ws.close(None).await.unwrap();
    let output = restarted.terminate_and_capture();
    mock.abort();
    assert_resources_released(&home, output.cleanup);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn round21_daemon_hard_cap_closes_both_plan_modes_and_retains_recovery() {
    let (mut daemon, mock) = round21_start_daemon("round21-hard-cap").await;
    let home = daemon.home.clone();
    let port = daemon.port;
    let (mut ws, _) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
            .await
            .unwrap();
    receive_history(&mut ws).await;
    for executing in [false, true] {
        ws.send(Message::Text(json!({"text":if executing {"R21_READY execution"} else {"R21_HARD planning"},"plan_mode":true}).to_string().into())).await.unwrap();
        let mut events = receive_run_events(&mut ws).await;
        if executing {
            let ready = events
                .iter()
                .find(|e| e["type"] == "plan_state" && e["plan"]["status"] == "ready")
                .unwrap()["plan"]
                .clone();
            ws.send(Message::Text(json!({"plan_action":{"action":"execute","plan_id":ready["plan_id"],"revision":ready["revision"]}}).to_string().into())).await.unwrap();
            events = receive_run_events(&mut ws).await;
        }
        assert_eq!(events.last().unwrap()["phase"], "hard_cap");
        assert_eq!(events.iter().filter(|e| e["type"] == "done").count(), 1);
        let plan = events
            .iter()
            .rev()
            .find(|e| e["type"] == "plan_state")
            .unwrap()["plan"]
            .clone();
        assert_eq!(plan["status"], "failed");
        assert_eq!(
            plan["execution_attempt"].as_u64().unwrap_or(0) > 0,
            executing
        );
        assert_eq!(
            round21_outcomes(&home).last().unwrap()["status"],
            "incomplete"
        );
        ws.close(None).await.unwrap();
        if executing {
            daemon.stop_preserving_home();
            daemon = spawn_lingclaw_in_home(home.clone(), port, &["--serve"], Stdio::null());
            let deadline = Instant::now() + Duration::from_secs(15);
            while lingclaw_health(port, deadline).is_err() {
                assert!(daemon.child.try_wait().unwrap().is_none());
                assert!(Instant::now() < deadline);
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        }
        (ws, _) =
            tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/ws?session=main"))
                .await
                .unwrap();
        let history = receive_history(&mut ws).await;
        assert!(
            history["plans"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["plan_id"] == plan["plan_id"] && p["status"] == "failed")
        );
        if executing {
            ws.send(Message::Text(json!({"plan_action":{"action":"resume","plan_id":plan["plan_id"],"revision":plan["revision"]}}).to_string().into())).await.unwrap();
            round21_wait_event(&mut ws, "start").await;
            ws.send(Message::Text("/stop".into())).await.unwrap();
            let resumed = receive_run_events(&mut ws).await;
            assert_eq!(resumed.last().unwrap()["phase"], "stopped");
        }
        ws.send(Message::Text(json!({"plan_action":{"action":"discard","plan_id":plan["plan_id"],"revision":plan["revision"]}}).to_string().into())).await.unwrap();
        assert_eq!(
            round21_wait_event(&mut ws, "plan_state").await["plan"]["status"],
            "discarded"
        );
    }
    ws.close(None).await.unwrap();
    let output = daemon.terminate_and_capture();
    mock.abort();
    assert_resources_released(&home, output.cleanup);
}
