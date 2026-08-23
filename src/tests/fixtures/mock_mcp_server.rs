use std::{
    env,
    fs::{self, OpenOptions},
    io::{self, BufRead, Write},
    path::PathBuf,
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

fn tools_list_response(
    id: &str,
    tool_name: &str,
    description: &str,
    read_only: bool,
    destructive: bool,
) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"tools\":[{{\"name\":\"{}\",\"description\":\"{}\",\"inputSchema\":{{\"type\":\"object\",\"properties\":{{}}}},\"annotations\":{{\"readOnlyHint\":{},\"destructiveHint\":{}}}}}]}}}}",
        id,
        tool_name,
        description,
        read_only,
        destructive,
    )
}

fn tools_call_response(id: &str, label: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}}}",
        id,
        label
    )
}

fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let mut path = String::from_utf8(decoded).ok()?;
    if cfg!(windows)
        && path.starts_with('/')
        && path.as_bytes().get(2).copied() == Some(b':')
    {
        path.remove(0);
    }
    Some(PathBuf::from(path))
}

fn resources_list_response(id: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"resources\":[]}}}}",
        id
    )
}

fn prompts_list_response(id: &str) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{{\"prompts\":[]}}}}",
        id
    )
}

fn main() {
    let mode = env::var("LINGCLAW_MCP_MODE").unwrap_or_else(|_| "default".to_string());
    let log_path = env::var("LINGCLAW_MCP_LOG").ok();
    append_log(log_path.as_deref(), "start");
    if let Ok(value) = env::var("LINGCLAW_MCP_ENV_CHECK") {
        append_log(log_path.as_deref(), &format!("env:LINGCLAW_MCP_ENV_CHECK={value}"));
    }

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

        if mode == "verify-roots-capability"
            && method.is_none()
            && id.as_deref() == Some("9100")
        {
            let identity = extract_string_field(trimmed, "uri")
                .and_then(|uri| file_uri_path(&uri))
                .and_then(|root| fs::read_to_string(root.join("identity.txt")).ok())
                .unwrap_or_else(|| "unavailable".to_string());
            append_log(
                log_path.as_deref(),
                &format!("roots-identity:{}", identity.trim()),
            );
            continue;
        }

        match method.as_deref() {
            Some("initialize") => {
                if let Some(id) = id.as_deref() {
                    write_line(&mut stdout, &initialize_response(id));
                }
            }
            Some("notifications/initialized") => {}
            Some("tools/list") => {
                tools_list_count += 1;
                let (tool_name, description) = if mode == "mutating" {
                    ("delete_issue", "Delete an issue")
                } else if mode == "tool-change" && tools_list_count >= 2 {
                    ("beta", "Retrieve the current value")
                } else {
                    ("alpha", "Retrieve the current value")
                };
                if let Some(id) = id.as_deref() {
                    if mode == "roots-id-collision" {
                        write_line(
                            &mut stdout,
                            &format!(
                                "{{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"roots/list\",\"params\":{{}}}}",
                                id
                            ),
                        );
                    }
                    let mutating = mode == "mutating";
                    write_line(
                        &mut stdout,
                        &tools_list_response(
                            id,
                            tool_name,
                            description,
                            !mutating,
                            mutating,
                        ),
                    );
                }
                if mode == "tool-change" && tools_list_count == 1 {
                    write_line(
                        &mut stdout,
                        "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\",\"params\":{}}",
                    );
                }
            }
            Some("tools/call") => {
                if mode == "delayed-roots" {
                    thread::sleep(Duration::from_millis(200));
                    append_log(log_path.as_deref(), "send:roots/list");
                    write_line(
                        &mut stdout,
                        "{\"jsonrpc\":\"2.0\",\"id\":9001,\"method\":\"roots/list\",\"params\":{}}",
                    );
                }
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
            Some("resources/list") => {
                if let Some(id) = id.as_deref() {
                    write_line(&mut stdout, &resources_list_response(id));
                }
            }
            Some("prompts/list") => {
                if let Some(id) = id.as_deref() {
                    write_line(&mut stdout, &prompts_list_response(id));
                }
            }
            _ => {}
        }
    }
}
