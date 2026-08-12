use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

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
    let port = reserve_ephemeral_port();
    let home = create_isolated_home(label);
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
