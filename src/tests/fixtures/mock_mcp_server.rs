use std::{
    env,
    fs::OpenOptions,
    io::{self, BufRead, Write},
    thread,
    time::Duration,
};

fn append_log(path: Option<&str>, line: &str) {
    let Some(path) = path else {
        return;
    };
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .expect("log file should open");
    writeln!(file, "{}", line).expect("log line should write");
}

fn extract_number_field(line: &str, field: &str) -> Option<String> {
    let needle = format!("\"{}\":", field);
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

fn extract_string_field(line: &str, field: &str) -> Option<String> {
    let needle = format!("\"{}\":\"", field);
    let start = line.find(&needle)? + needle.len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn write_line(stdout: &mut io::StdoutLock<'_>, line: &str) {
    stdout
        .write_all(line.as_bytes())
        .expect("stdout write should succeed");
    stdout
        .write_all(b"\n")
        .expect("stdout newline should succeed");
    stdout.flush().expect("stdout flush should succeed");
}

fn initialize_response(id: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{{\"tools\":{{\"listChanged\":true}},\"roots\":{{\"listChanged\":false}}}},\"serverInfo\":{{\"name\":\"mock\",\"version\":\"1.0\"}}}}}}",
        id
    )
}

fn tools_list_response(id: &str, tool_name: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"tools\":[{{\"name\":\"{}\",\"description\":\"mock tool\",\"inputSchema\":{{\"type\":\"object\",\"properties\":{{}}}}}}]}}}}",
        id,
        tool_name
    )
}

fn tools_call_response(id: &str, label: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}}}",
        id,
        label
    )
}

fn main() {
    let mode = env::var("LINGCLAW_MCP_MODE").unwrap_or_else(|_| "default".to_string());
    let log_path = env::var("LINGCLAW_MCP_LOG").ok();
    append_log(log_path.as_deref(), "start");

    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut tools_list_count = 0usize;

    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        append_log(log_path.as_deref(), &format!("recv:{}", trimmed));

        let method = extract_string_field(trimmed, "method");
        let id = extract_number_field(trimmed, "id");

        match method.as_deref() {
            Some("initialize") => {
                if let Some(id) = id.as_deref() {
                    write_line(&mut stdout, &initialize_response(id));
                }
            }
            Some("notifications/initialized") => {}
            Some("tools/list") => {
                tools_list_count += 1;
                let tool_name = if mode == "tool-change" && tools_list_count >= 2 {
                    "beta"
                } else {
                    "alpha"
                };
                if let Some(id) = id.as_deref() {
                    write_line(&mut stdout, &tools_list_response(id, tool_name));
                }
                if mode == "tool-change" && tools_list_count == 1 {
                    write_line(
                        &mut stdout,
                        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\",\"params\":{}}",
                    );
                }
            }
            Some("tools/call") => {
                if mode == "concurrent" {
                    thread::sleep(Duration::from_millis(50));
                }
                let label = if trimmed.contains("\"value\":\"left\"") {
                    "left"
                } else if trimmed.contains("\"value\":\"right\"") {
                    "right"
                } else {
                    "ok"
                };
                if let Some(id) = id.as_deref() {
                    write_line(&mut stdout, &tools_call_response(id, label));
                }
                if mode == "restart-once" {
                    break;
                }
            }
            _ => {}
        }
    }
}
