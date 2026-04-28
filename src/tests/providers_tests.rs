use super::*;
use crate::ImageAttachment;
use crate::config::S3Config;
use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct CapturedHttpRequest {
    request_line: String,
    headers: std::collections::HashMap<String, String>,
    body: String,
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

fn parse_content_length(header_text: &str) -> usize {
    header_text
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

fn read_http_request(stream: &mut TcpStream) -> CapturedHttpRequest {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout should be set");

    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];

    loop {
        let read = stream.read(&mut chunk).expect("request should be readable");
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);

        if let Some(headers_end) = find_headers_end(&buffer) {
            let header_text = String::from_utf8_lossy(&buffer[..headers_end + 4]);
            let content_length = parse_content_length(&header_text);
            let total_len = headers_end + 4 + content_length;
            if buffer.len() >= total_len {
                break;
            }
        }
    }

    let headers_end = find_headers_end(&buffer).expect("request should contain headers");
    let header_text = String::from_utf8_lossy(&buffer[..headers_end]).to_string();
    let mut lines = header_text.lines();
    let request_line = lines.next().expect("request line should exist").to_string();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let body = String::from_utf8_lossy(&buffer[headers_end + 4..]).to_string();

    CapturedHttpRequest {
        request_line,
        headers,
        body,
    }
}

fn spawn_one_shot_http_server(
    response_content_type: &'static str,
    response_body: String,
) -> (
    String,
    mpsc::Receiver<CapturedHttpRequest>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
    let address = listener
        .local_addr()
        .expect("listener should expose address");
    let (request_tx, request_rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("request should connect");
        let request = read_http_request(&mut stream);
        request_tx
            .send(request)
            .expect("captured request should be sent");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {response_content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        stream
            .write_all(response.as_bytes())
            .expect("response should be written");
        stream.flush().expect("response should flush");
    });

    (format!("http://{}", address), request_rx, handle)
}

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time should be valid")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{nanos}"))
}

#[test]
fn think_level_to_reasoning_effort_all_levels() {
    assert_eq!(think_level_to_reasoning_effort("minimal"), "low");
    assert_eq!(think_level_to_reasoning_effort("low"), "low");
    assert_eq!(think_level_to_reasoning_effort("medium"), "medium");
    assert_eq!(think_level_to_reasoning_effort("high"), "high");
    assert_eq!(think_level_to_reasoning_effort("xhigh"), "high");
    assert_eq!(think_level_to_reasoning_effort("unknown"), "medium");
    assert_eq!(think_level_to_reasoning_effort("auto"), "medium");
}

#[test]
fn think_level_to_budget_all_levels() {
    assert_eq!(think_level_to_budget("minimal"), 1024);
    assert_eq!(think_level_to_budget("low"), 4096);
    assert_eq!(think_level_to_budget("medium"), 10240);
    assert_eq!(think_level_to_budget("high"), 16384);
    assert_eq!(think_level_to_budget("xhigh"), 32768);
    assert_eq!(think_level_to_budget("unknown"), 10240);
}

#[test]
fn think_level_to_deepseek_reasoning_effort_all_levels() {
    assert_eq!(think_level_to_deepseek_reasoning_effort("minimal"), "high");
    assert_eq!(think_level_to_deepseek_reasoning_effort("low"), "high");
    assert_eq!(think_level_to_deepseek_reasoning_effort("medium"), "high");
    assert_eq!(think_level_to_deepseek_reasoning_effort("high"), "high");
    assert_eq!(think_level_to_deepseek_reasoning_effort("xhigh"), "max");
    assert_eq!(think_level_to_deepseek_reasoning_effort("unknown"), "high");
    assert_eq!(think_level_to_deepseek_reasoning_effort("auto"), "high");
}

#[test]
fn drain_sse_lines_preserves_partial_tail() {
    let mut partial = String::new();

    let first = drain_sse_lines(&mut partial, "data: one\ndata: two");
    assert_eq!(first, vec!["data: one".to_string()]);
    assert_eq!(partial, "data: two");

    let second = drain_sse_lines(&mut partial, "\ndata: three\n");
    assert_eq!(
        second,
        vec!["data: two".to_string(), "data: three".to_string()]
    );
    assert!(partial.is_empty());
}

#[test]
fn convert_messages_to_openai_all_roles() {
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("you are helpful".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("hi".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("result".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "unknown_role".into(),
            content: Some("skip me".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    let out = convert_messages_to_openai_with_options(&messages, false, None);
    assert_eq!(out.len(), 4); // unknown_role skipped
    assert_eq!(out[0]["role"], "system");
    assert_eq!(out[1]["role"], "user");
    assert_eq!(out[2]["role"], "assistant");
    assert_eq!(out[3]["role"], "tool");
    assert_eq!(out[3]["tool_call_id"], "tc1");
}

#[test]
fn convert_messages_to_openai_assistant_with_tool_calls() {
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            gemini_thought_signature: Some("sig-gemini".into()),
            function: FunctionCall {
                name: "exec".into(),
                arguments: r#"{"cmd":"ls"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    }];
    let out = convert_messages_to_openai_with_options(&messages, false, None);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["content"], "");
    assert!(out[0]["tool_calls"].is_array());
    assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "exec");
    assert!(
        !out[0]["tool_calls"][0]
            .as_object()
            .unwrap()
            .contains_key("gemini_thought_signature")
    );
}

#[test]
fn convert_messages_to_openai_allows_null_tool_call_content_when_requested() {
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "exec".into(),
                arguments: r#"{"cmd":"ls"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    }];

    let out = convert_messages_to_openai_with_options(&messages, true, None);

    assert_eq!(out.len(), 1);
    assert!(out[0]["content"].is_null());
}

#[test]
fn convert_messages_to_openai_deepseek_v4_includes_reasoning_content() {
    let messages = vec![
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("let me think".into()),
            images: None,
            thinking: Some("I need to reason about this".into()),
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    // Without deepseek-v4 format: no reasoning_content in output
    let out_plain = convert_messages_to_openai_with_options(&messages, false, None);
    assert!(out_plain[1].get("reasoning_content").is_none());

    // With deepseek-v4 format: reasoning_content is included
    let out_ds = convert_messages_to_openai_with_options(&messages, false, Some("deepseek-v4"));
    assert_eq!(
        out_ds[1]["reasoning_content"].as_str(),
        Some("I need to reason about this")
    );
}

#[test]
fn convert_messages_to_openai_deepseek_v4_omits_empty_reasoning_content() {
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: Some("answer".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    let out = convert_messages_to_openai_with_options(&messages, false, Some("deepseek-v4"));
    assert_eq!(
        out[0]["reasoning_content"].as_str(),
        Some("Historical assistant response replayed without original reasoning_content.")
    );
}

#[test]
fn convert_messages_to_anthropic_system_extraction() {
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("system prompt".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    let (system, out) = convert_messages_to_anthropic(&messages);
    assert_eq!(system, "system prompt");
    assert_eq!(out.len(), 1); // system not in messages
    assert_eq!(out[0]["role"], "user");
}

#[test]
fn convert_messages_to_anthropic_tool_as_user_message() {
    let messages = vec![ChatMessage {
        role: "tool".into(),
        content: Some("file contents".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: Some("tc1".into()),
        timestamp: None,
    }];
    let (_, out) = convert_messages_to_anthropic(&messages);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["role"], "user");
    assert_eq!(out[0]["content"][0]["type"], "tool_result");
    assert_eq!(out[0]["content"][0]["tool_use_id"], "tc1");
}

#[test]
fn convert_messages_to_anthropic_assistant_with_tool_use() {
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: Some("let me check".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            gemini_thought_signature: Some("sig-gemini".into()),
            function: FunctionCall {
                name: "exec".into(),
                arguments: r#"{"cmd":"ls"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    }];
    let (_, out) = convert_messages_to_anthropic(&messages);
    assert_eq!(out.len(), 1);
    let content = out[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2); // text block + tool_use block
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "tool_use");
    assert_eq!(content[1]["name"], "exec");
    assert!(
        !content[1]
            .as_object()
            .unwrap()
            .contains_key("gemini_thought_signature")
    );
}

#[test]
fn convert_messages_to_anthropic_roundtrips_structured_thinking_blocks() {
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: Some("answer".into()),
        images: None,
        thinking: Some("visible thinking".into()),
        anthropic_thinking_blocks: Some(vec![
            crate::AnthropicThinkingBlock {
                block_type: "thinking".into(),
                thinking: Some("hidden reasoning".into()),
                signature: Some("sig_123".into()),
                data: None,
            },
            crate::AnthropicThinkingBlock {
                block_type: "redacted_thinking".into(),
                thinking: None,
                signature: None,
                data: Some("opaque_blob".into()),
            },
        ]),
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    let (_, out) = convert_messages_to_anthropic(&messages);
    let content = out[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 3);
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], "hidden reasoning");
    assert_eq!(content[0]["signature"], "sig_123");
    assert_eq!(content[1]["type"], "redacted_thinking");
    assert_eq!(content[1]["data"], "opaque_blob");
    assert_eq!(content[2]["type"], "text");
    assert_eq!(content[2]["text"], "answer");
}

#[test]
fn convert_messages_to_anthropic_empty_assistant_gets_placeholder() {
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let (_, out) = convert_messages_to_anthropic(&messages);
    let content = out[0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "");
}

#[test]
fn convert_messages_to_ollama_all_roles() {
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("system prompt".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("checking".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                gemini_thought_signature: Some("sig-gemini".into()),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("done".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
    ];

    let out = convert_messages_to_ollama(&messages, &std::collections::HashMap::new());

    assert_eq!(out.len(), 4);
    assert_eq!(out[0]["role"], "system");
    assert_eq!(out[1]["role"], "user");
    assert_eq!(out[2]["tool_calls"][0]["type"], "function");
    assert_eq!(out[2]["tool_calls"][0]["id"], "tc1");
    assert!(
        !out[2]["tool_calls"][0]
            .as_object()
            .unwrap()
            .contains_key("gemini_thought_signature")
    );
    assert_eq!(out[2]["tool_calls"][0]["function"]["index"], 0);
    assert_eq!(out[2]["tool_calls"][0]["function"]["name"], "read_file");
    assert_eq!(
        out[2]["tool_calls"][0]["function"]["arguments"]["path"],
        "README.md"
    );
    assert_eq!(out[3]["role"], "tool");
    assert_eq!(out[3]["tool_name"], "read_file");
}

#[test]
fn build_llm_response_empty_content_and_no_tools() {
    let resp = build_llm_response(String::new(), String::new(), vec![], None, None).unwrap();
    assert!(resp.message.content.is_none());
    assert!(resp.message.tool_calls.is_none());
    assert!(resp.message.thinking.is_none());
    assert_eq!(resp.message.role, "assistant");
}

#[test]
fn build_llm_response_thinking_buf_stored() {
    let resp =
        build_llm_response("reply".into(), "deep reasoning".into(), vec![], None, None).unwrap();
    assert_eq!(resp.message.content.as_deref(), Some("reply"));
    assert_eq!(resp.message.thinking.as_deref(), Some("deep reasoning"));
}

#[test]
fn build_anthropic_llm_response_stores_roundtrip_blocks() {
    let resp = build_anthropic_llm_response(
        "reply".into(),
        "deep reasoning".into(),
        vec![
            crate::AnthropicThinkingBlock {
                block_type: "thinking".into(),
                thinking: Some("deep reasoning".into()),
                signature: Some("sig_abc".into()),
                data: None,
            },
            crate::AnthropicThinkingBlock {
                block_type: "redacted_thinking".into(),
                thinking: None,
                signature: None,
                data: Some("opaque".into()),
            },
        ],
        vec![],
        None,
        None,
    )
    .unwrap();

    let blocks = resp.message.anthropic_thinking_blocks.unwrap();
    assert_eq!(resp.message.thinking.as_deref(), Some("deep reasoning"));
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].block_type, "thinking");
    assert_eq!(blocks[0].signature.as_deref(), Some("sig_abc"));
    assert_eq!(blocks[1].block_type, "redacted_thinking");
    assert_eq!(blocks[1].data.as_deref(), Some("opaque"));
}

#[test]
fn build_llm_response_empty_thinking_buf_is_none() {
    let resp = build_llm_response("reply".into(), String::new(), vec![], None, None).unwrap();
    assert_eq!(resp.message.content.as_deref(), Some("reply"));
    assert!(resp.message.thinking.is_none());
}

#[test]
fn build_llm_response_with_content_and_tools() {
    let resp = build_llm_response(
        "thinking...".into(),
        String::new(),
        vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "exec".into(),
                arguments: "{}".into(),
            },
        }],
        Some(123),
        Some(45),
    )
    .unwrap();
    assert_eq!(resp.message.content.as_deref(), Some("thinking..."));
    assert_eq!(resp.message.tool_calls.as_ref().unwrap().len(), 1);
    assert_eq!(resp.input_tokens, Some(123));
    assert_eq!(resp.output_tokens, Some(45));
}

#[test]
fn normalize_tool_call_ids_whitespace_only_id_gets_fallback() {
    let resp = build_llm_response(
        String::new(),
        String::new(),
        vec![ToolCall {
            id: "   ".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "search".into(),
                arguments: "{}".into(),
            },
        }],
        None,
        None,
    )
    .unwrap();
    let tool_calls = resp.message.tool_calls.expect("tool calls should exist");
    assert!(
        tool_calls[0].id.starts_with("tool_call_"),
        "whitespace-only id should get fallback, got: {}",
        tool_calls[0].id
    );
}

#[test]
fn build_llm_response_assigns_unique_fallback_tool_ids() {
    let resp = build_llm_response(
        String::new(),
        String::new(),
        vec![
            ToolCall {
                id: String::new(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "mcp__search".into(),
                    arguments: "{}".into(),
                },
            },
            ToolCall {
                id: "dup".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            },
            ToolCall {
                id: "dup".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "grep_search".into(),
                    arguments: "{}".into(),
                },
            },
        ],
        None,
        None,
    )
    .unwrap();

    let tool_calls = resp.message.tool_calls.expect("tool calls should exist");
    let ids: std::collections::HashSet<&str> = tool_calls
        .iter()
        .map(|tool_call| tool_call.id.as_str())
        .collect();

    assert_eq!(tool_calls[1].id, "dup");
    assert_eq!(ids.len(), tool_calls.len());
    assert!(tool_calls[0].id.starts_with("tool_call_"));
    assert!(tool_calls[2].id.starts_with("tool_call_"));
}

#[test]
fn total_anthropic_input_tokens_sums_cache_components() {
    let usage = AnthropicUsage {
        input_tokens: Some(100),
        output_tokens: Some(50),
        cache_creation_input_tokens: Some(20),
        cache_read_input_tokens: Some(30),
    };

    assert_eq!(total_anthropic_input_tokens(&usage), 150);
}

#[test]
fn anthropic_system_payload_uses_structured_cache_blocks_when_enabled() {
    let system_prompt = "You are a helpful assistant.";
    let system_val = anthropic_system_payload(system_prompt, true);
    let blocks = system_val.as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[0]["text"], system_prompt);
    assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
}

#[test]
fn anthropic_system_payload_stays_plain_string_when_disabled() {
    let system_val = anthropic_system_payload("You are a helpful assistant.", false);
    assert_eq!(system_val.as_str(), Some("You are a helpful assistant."));
}

#[test]
fn anthropic_system_payload_splits_at_environment_delimiter() {
    let system_prompt =
        format!("Stable persona and tools.{delim} Linux\n- Current system local time: 2026-04-28 14:30 +08:00",
                delim = super::ENV_BLOCK_DELIMITER);
    let system_val = anthropic_system_payload(&system_prompt, true);
    let blocks = system_val.as_array().unwrap();
    assert_eq!(blocks.len(), 2, "should split into stable (cached) and dynamic (uncached) blocks");
    assert_eq!(blocks[0]["type"], "text");
    assert_eq!(blocks[0]["text"], "Stable persona and tools.");
    assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
    assert_eq!(blocks[1]["type"], "text");
    assert_eq!(
        blocks[1]["text"],
        format!("{delim} Linux\n- Current system local time: 2026-04-28 14:30 +08:00",
                delim = super::ENV_BLOCK_DELIMITER),
    );
    assert!(blocks[1].get("cache_control").is_none());
}

#[test]
fn anthropic_system_payload_no_split_without_environment_block() {
    let system_val = anthropic_system_payload("Just a simple prompt.", true);
    let blocks = system_val.as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
}

#[test]
fn anthropic_tools_last_has_cache_control_when_enabled() {
    let mut tools: Vec<serde_json::Value> = vec![
        json!({"name": "tool_a", "description": "A"}),
        json!({"name": "tool_b", "description": "B"}),
    ];
    maybe_apply_anthropic_tool_cache_control(&mut tools, true);
    assert!(tools[0].get("cache_control").is_none());
    assert_eq!(tools[1]["cache_control"]["type"], "ephemeral");
}

#[test]
fn anthropic_tools_do_not_add_cache_control_when_disabled() {
    let mut tools: Vec<serde_json::Value> = vec![
        json!({"name": "tool_a", "description": "A"}),
        json!({"name": "tool_b", "description": "B"}),
    ];
    maybe_apply_anthropic_tool_cache_control(&mut tools, false);
    assert!(tools[0].get("cache_control").is_none());
    assert!(tools[1].get("cache_control").is_none());
}

#[test]
fn anthropic_thinking_does_not_inflate_max_tokens() {
    let resolved = ResolvedModel {
        provider: Provider::Anthropic,
        api_base: "https://api.anthropic.com".into(),
        api_key: "test-key".into(),
        model_id: "claude-sonnet-test".into(),
        reasoning: true,
        thinking_format: None,
        max_tokens: Some(128_000),
        context_window: 200_000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("You are helpful.".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let body = build_anthropic_stream_body(&resolved, &messages, None, "medium", &[], true)
        .expect("body should build");

    assert_eq!(body["max_tokens"].as_u64(), Some(128_000));
    assert_eq!(body["thinking"]["budget_tokens"].as_u64(), Some(10_240));
}

/// When max_tokens is smaller than the raw thinking budget, budget_tokens must be
/// clamped to (max_tokens - 1024) so that max_tokens > budget_tokens holds.
#[test]
fn anthropic_thinking_clamps_budget_when_max_tokens_is_small() {
    let resolved = ResolvedModel {
        provider: Provider::Anthropic,
        api_base: "https://api.anthropic.com".into(),
        api_key: "test-key".into(),
        model_id: "claude-sonnet-test".into(),
        reasoning: true,
        thinking_format: None,
        max_tokens: Some(4_096), // smaller than medium budget (10240)
        context_window: 200_000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hello".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    let body = build_anthropic_stream_body(&resolved, &messages, None, "medium", &[], true)
        .expect("body should build");

    assert_eq!(body["max_tokens"].as_u64(), Some(4_096));
    // budget clamped to 4096 - 1024 = 3072, which is >= 1024 so thinking stays enabled
    assert_eq!(body["thinking"]["budget_tokens"].as_u64(), Some(3_072));
}

/// When max_tokens is too small to accommodate even the minimum 1024-token thinking
/// budget, the thinking block must be omitted entirely to avoid an Anthropic 400.
#[test]
fn anthropic_thinking_disabled_when_max_tokens_too_small() {
    let resolved = ResolvedModel {
        provider: Provider::Anthropic,
        api_base: "https://api.anthropic.com".into(),
        api_key: "test-key".into(),
        model_id: "claude-sonnet-test".into(),
        reasoning: true,
        thinking_format: None,
        max_tokens: Some(1_024), // 1024 - 1024 = 0 < 1024 minimum
        context_window: 200_000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hello".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    let body = build_anthropic_stream_body(&resolved, &messages, None, "medium", &[], true)
        .expect("body should build");

    assert_eq!(body["max_tokens"].as_u64(), Some(1_024));
    // thinking block must be absent — budget would be 0 which violates the >=1024 minimum
    assert!(body["thinking"].is_null());
}

#[test]
fn process_openai_data_line_reports_done_marker() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let live_tx: LiveTx = tx;

    let done = rt.block_on(async {
        let mut state = OpenAiStreamState {
            content_buf: String::new(),
            thinking_buf: String::new(),
            tool_calls: Vec::new(),
            input_tokens: None,
            output_tokens: None,
            client_gone: false,
            reasoning_started: false,
        };
        process_openai_data_line("[DONE]", &live_tx, &mut state).await
    });

    assert!(done);
    assert!(rx.try_recv().is_err());
}

#[test]
fn process_openai_data_line_keeps_reasoning_open_for_empty_content_delta() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let live_tx: LiveTx = tx;

    rt.block_on(async {
        let mut state = OpenAiStreamState {
            content_buf: String::new(),
            thinking_buf: String::new(),
            tool_calls: Vec::new(),
            input_tokens: None,
            output_tokens: None,
            client_gone: false,
            reasoning_started: false,
        };

        process_openai_data_line(
            r#"{"choices":[{"delta":{"reasoning_content":"用户","content":""}}]}"#,
            &live_tx,
            &mut state,
        )
        .await;
        process_openai_data_line(
            r#"{"choices":[{"delta":{"reasoning_content":"现在","content":""}}]}"#,
            &live_tx,
            &mut state,
        )
        .await;

        assert!(state.reasoning_started);
        assert!(state.content_buf.is_empty());

        process_openai_data_line(
            r#"{"choices":[{"delta":{"content":"answer"}}]}"#,
            &live_tx,
            &mut state,
        )
        .await;

        assert!(!state.reasoning_started);
        assert_eq!(state.content_buf, "answer");
    });

    let mut event_types = Vec::new();
    while let Ok(event) = rx.try_recv() {
        event_types.push(
            event["type"]
                .as_str()
                .expect("event type should be present")
                .to_string(),
        );
    }

    assert_eq!(
        event_types,
        vec![
            "thinking_start".to_string(),
            "thinking_delta".to_string(),
            "thinking_delta".to_string(),
            "thinking_done".to_string(),
            "delta".to_string(),
        ]
    );
}

#[test]
fn process_anthropic_sse_line_keeps_event_type_between_lines() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let live_tx: LiveTx = tx;

    let content = rt.block_on(async {
        let mut state = AnthropicStreamState {
            current_event_type: String::new(),
            content_buf: String::new(),
            thinking_buf: String::new(),
            thinking_blocks: Vec::new(),
            tool_calls: Vec::new(),
            input_tokens: None,
            output_tokens: None,
            block_tool_idx: HashMap::new(),
            block_thinking_idx: HashMap::new(),
            client_gone: false,
            reasoning_started: false,
            thinking_block_idx: None,
        };

        process_anthropic_sse_line("event: content_block_delta", &live_tx, &mut state).await;

        process_anthropic_sse_line(
            r#"data: {"delta":{"type":"text_delta","text":"tail"},"index":0}"#,
            &live_tx,
            &mut state,
        )
        .await;

        state.content_buf
    });

    assert_eq!(content, "tail");
    assert!(rx.try_recv().is_ok());
}

#[test]
fn process_anthropic_sse_line_captures_signature_and_redacted_blocks() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let live_tx: LiveTx = tx;

    let state = rt.block_on(async {
        let mut state = AnthropicStreamState {
            current_event_type: String::new(),
            content_buf: String::new(),
            thinking_buf: String::new(),
            thinking_blocks: Vec::new(),
            tool_calls: Vec::new(),
            input_tokens: None,
            output_tokens: None,
            block_tool_idx: HashMap::new(),
            block_thinking_idx: HashMap::new(),
            client_gone: false,
            reasoning_started: false,
            thinking_block_idx: None,
        };

        process_anthropic_sse_line(
            r#"event: content_block_start"#,
            &live_tx,
            &mut state,
        )
        .await;
        process_anthropic_sse_line(
            r#"data: {"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            &live_tx,
            &mut state,
        )
        .await;
        process_anthropic_sse_line(r#"event: content_block_delta"#, &live_tx, &mut state).await;
        process_anthropic_sse_line(
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}"#,
            &live_tx,
            &mut state,
        )
        .await;
        process_anthropic_sse_line(r#"event: content_block_delta"#, &live_tx, &mut state).await;
        process_anthropic_sse_line(
            r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig_123"}}"#,
            &live_tx,
            &mut state,
        )
        .await;
        process_anthropic_sse_line(r#"event: content_block_start"#, &live_tx, &mut state).await;
        process_anthropic_sse_line(
            r#"data: {"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"opaque_blob"}}"#,
            &live_tx,
            &mut state,
        )
        .await;

        state
    });

    assert_eq!(state.thinking_buf, "plan");
    assert_eq!(state.thinking_blocks.len(), 2);
    assert_eq!(state.thinking_blocks[0].block_type, "thinking");
    assert_eq!(state.thinking_blocks[0].thinking.as_deref(), Some("plan"));
    assert_eq!(
        state.thinking_blocks[0].signature.as_deref(),
        Some("sig_123")
    );
    assert_eq!(state.thinking_blocks[1].block_type, "redacted_thinking");
    assert_eq!(
        state.thinking_blocks[1].data.as_deref(),
        Some("opaque_blob")
    );
}

#[tokio::test]
async fn build_ollama_stream_body_includes_tools_think_and_num_predict() {
    let resolved = ResolvedModel {
        provider: Provider::Ollama,
        api_base: "http://127.0.0.1:11434".into(),
        api_key: String::new(),
        model_id: "qwen3".into(),
        reasoning: true,
        thinking_format: Some("ollama".into()),
        max_tokens: Some(256),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hello".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    let workspace = unique_temp_dir("lingclaw-ollama-body");
    let body = build_ollama_stream_body(&resolved, &messages, &workspace, None, "high", &[], true)
        .await
        .unwrap();

    assert_eq!(body["model"], "qwen3");
    assert_eq!(body["stream"], true);
    assert_eq!(body["think"], true);
    assert_eq!(body["options"]["num_predict"], 256);
    assert_eq!(body["options"]["num_ctx"], 128000);
    assert!(body["tools"].is_array());
}

#[tokio::test]
async fn build_ollama_stream_body_uses_levels_for_gpt_oss() {
    let resolved = ResolvedModel {
        provider: Provider::Ollama,
        api_base: "http://127.0.0.1:11434".into(),
        api_key: String::new(),
        model_id: "gpt-oss:20b".into(),
        reasoning: true,
        thinking_format: Some("gpt-oss".into()),
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };

    let workspace = unique_temp_dir("lingclaw-ollama-levels");
    let body = build_ollama_stream_body(&resolved, &[], &workspace, None, "high", &[], true)
        .await
        .unwrap();

    assert_eq!(body["think"], "high");
}

#[test]
fn with_optional_bearer_auth_skips_header_for_empty_key() {
    let client = reqwest::Client::new();
    let request = with_optional_bearer_auth(client.post("http://localhost/test"), "")
        .build()
        .expect("request should build");

    assert!(
        request
            .headers()
            .get(reqwest::header::AUTHORIZATION)
            .is_none()
    );
}

#[test]
fn with_optional_bearer_auth_sets_header_for_non_empty_key() {
    let client = reqwest::Client::new();
    let request = with_optional_bearer_auth(client.post("http://localhost/test"), "secret")
        .build()
        .expect("request should build");

    assert_eq!(
        request.headers().get(reqwest::header::AUTHORIZATION),
        Some(&reqwest::header::HeaderValue::from_static("Bearer secret"))
    );
}

#[test]
fn build_openai_stream_body_uses_null_tool_call_content_for_official_api() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: Provider::OpenAI.default_api_base().into(),
        api_key: "openai-key".into(),
        model_id: "gpt-4o-mini".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(128),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"README.md"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    }];

    let body = build_openai_stream_body(&resolved, &messages, None, "off", &[], true)
        .expect("body builds");

    assert!(body["messages"][0]["content"].is_null());
}

#[test]
fn build_openai_stream_body_keeps_string_tool_call_content_for_compatible_api() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://vip.aipro.love/v1".into(),
        api_key: "openai-key".into(),
        model_id: "gpt-5.4".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(128),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"README.md"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    }];

    let body = build_openai_stream_body(&resolved, &messages, None, "off", &[], true)
        .expect("body builds");

    assert_eq!(body["messages"][0]["content"], "");
}

#[test]
fn build_openai_stream_body_deepseek_v4_sends_thinking_and_reasoning_effort() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://api.deepseek.com/v1".into(),
        api_key: "deepseek-key".into(),
        model_id: "deepseek-v4-pro".into(),
        reasoning: true,
        thinking_format: Some("deepseek-v4".into()),
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("thinking step".into()),
            images: None,
            thinking: Some("I reasoned about this".into()),
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let body = build_openai_stream_body(&resolved, &messages, None, "high", &[], true)
        .expect("body builds");

    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["thinking"]["type"], "enabled");
    // reasoning_content must be present for tool-call assistant messages
    assert_eq!(
        body["messages"][1]["reasoning_content"].as_str(),
        Some("I reasoned about this")
    );
}

#[test]
fn build_openai_stream_body_deepseek_v4_replays_missing_reasoning_tool_turn_as_text() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://api.deepseek.com/v1".into(),
        api_key: "deepseek-key".into(),
        model_id: "deepseek-v4-pro".into(),
        reasoning: true,
        thinking_format: Some("deepseek-v4".into()),
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![
        ChatMessage {
            role: "user".into(),
            content: Some("continue".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("README contents".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            timestamp: None,
        },
    ];

    let body = build_openai_stream_body(&resolved, &messages, None, "high", &[], true)
        .expect("body builds");

    let body_messages = body["messages"]
        .as_array()
        .expect("messages should serialize as an array");
    assert_eq!(body_messages.len(), 2);
    assert_eq!(body_messages[1]["role"], "assistant");
    assert!(body_messages[1].get("tool_calls").is_none());
    assert_eq!(
        body_messages[1]["reasoning_content"].as_str(),
        Some(
            "Historical tool turn summarized because the original DeepSeek response omitted reasoning_content."
        )
    );
    let summary = body_messages[1]["content"]
        .as_str()
        .expect("replayed assistant summary should be text");
    assert!(summary.contains("DeepSeek omitted reasoning_content"));
    assert!(summary.contains("Tool read_file args: {\"path\":\"README.md\"}"));
    assert!(summary.contains("Tool result: README contents"));
}

#[tokio::test]
async fn build_openai_stream_body_deepseek_v4_keeps_reasoning_after_parallel_tool_calls() {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let live_tx: LiveTx = tx;
    let mut state = OpenAiStreamState {
        content_buf: String::new(),
        thinking_buf: String::new(),
        tool_calls: Vec::new(),
        input_tokens: None,
        output_tokens: None,
        client_gone: false,
        reasoning_started: false,
    };

    let thinking_chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "reasoning_content": "plan both reads"
            }
        }]
    })
    .to_string();
    process_openai_data_line(&thinking_chunk, &live_tx, &mut state).await;

    let tool_chunk = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [
                    {
                        "index": 0,
                        "id": "call_1",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"README.md\"}"
                        }
                    },
                    {
                        "index": 1,
                        "id": "call_2",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\":\"Cargo.toml\"}"
                        }
                    }
                ]
            },
            "finish_reason": "tool_calls"
        }]
    })
    .to_string();
    process_openai_data_line(&tool_chunk, &live_tx, &mut state).await;

    let response = build_llm_response(
        state.content_buf,
        state.thinking_buf,
        state.tool_calls,
        None,
        None,
    )
    .expect("response should build");

    assert_eq!(
        response.message.thinking.as_deref(),
        Some("plan both reads")
    );
    assert_eq!(
        response
            .message
            .tool_calls
            .as_ref()
            .expect("tool calls should exist")
            .len(),
        2
    );

    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://api.deepseek.com/v1".into(),
        api_key: "deepseek-key".into(),
        model_id: "deepseek-v4-pro".into(),
        reasoning: true,
        thinking_format: Some("deepseek-v4".into()),
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![
        ChatMessage {
            role: "user".into(),
            content: Some("compare both files".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        response.message.clone(),
        ChatMessage {
            role: "tool".into(),
            content: Some("README contents".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("Cargo contents".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("call_2".into()),
            timestamp: None,
        },
    ];

    let body = build_openai_stream_body(&resolved, &messages, None, "high", &[], true)
        .expect("body builds");

    assert_eq!(
        body["messages"][1]["reasoning_content"].as_str(),
        Some("plan both reads")
    );
    assert_eq!(
        body["messages"][1]["tool_calls"]
            .as_array()
            .expect("assistant tool calls should be serialized")
            .len(),
        2
    );
    assert_eq!(body["messages"][2]["tool_call_id"], "call_1");
    assert_eq!(body["messages"][3]["tool_call_id"], "call_2");
}

#[test]
fn build_openai_stream_body_deepseek_v4_off_keeps_reasoningless_tool_turn_structured() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://api.deepseek.com/v1".into(),
        api_key: "deepseek-key".into(),
        model_id: "deepseek-v4-pro".into(),
        reasoning: true,
        thinking_format: Some("deepseek-v4".into()),
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![
        ChatMessage {
            role: "user".into(),
            content: Some("continue".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("README contents".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            timestamp: None,
        },
    ];

    let body = build_openai_stream_body(&resolved, &messages, None, "off", &[], true)
        .expect("body builds");

    let body_messages = body["messages"]
        .as_array()
        .expect("messages should serialize as an array");
    assert_eq!(body_messages.len(), 3);
    assert_eq!(
        body_messages[1]["tool_calls"]
            .as_array()
            .expect("tool calls should stay structured")
            .len(),
        1
    );
    assert_eq!(body_messages[2]["role"], "tool");
}

#[test]
fn build_openai_stream_body_non_deepseek_keeps_reasoningless_tool_turn_structured() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://example-openai-compatible.invalid/v1".into(),
        api_key: "api-key".into(),
        model_id: "gpt-4.1".into(),
        reasoning: true,
        thinking_format: None,
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![
        ChatMessage {
            role: "user".into(),
            content: Some("continue".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("README contents".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            timestamp: None,
        },
    ];

    let body = build_openai_stream_body(&resolved, &messages, None, "high", &[], true)
        .expect("body builds");

    let body_messages = body["messages"]
        .as_array()
        .expect("messages should serialize as an array");
    assert_eq!(body_messages.len(), 3);
    assert_eq!(
        body_messages[1]["tool_calls"]
            .as_array()
            .expect("tool calls should stay structured")
            .len(),
        1
    );
    assert_eq!(body_messages[2]["role"], "tool");
}

#[test]
fn build_openai_stream_body_deepseek_v4_maps_xhigh_to_max() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://api.deepseek.com/v1".into(),
        api_key: "deepseek-key".into(),
        model_id: "deepseek-v4-pro".into(),
        reasoning: true,
        thinking_format: Some("deepseek-v4".into()),
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };

    let body =
        build_openai_stream_body(&resolved, &[], None, "xhigh", &[], true).expect("body builds");

    assert_eq!(body["reasoning_effort"], "max");
    assert_eq!(body["thinking"]["type"], "enabled");
}

#[test]
fn build_openai_stream_body_deepseek_v4_maps_low_to_high() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://api.deepseek.com/v1".into(),
        api_key: "deepseek-key".into(),
        model_id: "deepseek-v4-pro".into(),
        reasoning: true,
        thinking_format: Some("deepseek-v4".into()),
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };

    let body =
        build_openai_stream_body(&resolved, &[], None, "low", &[], true).expect("body builds");

    assert_eq!(body["reasoning_effort"], "high");
}

#[test]
fn build_openai_stream_body_deepseek_v4_off_sends_thinking_disabled() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://api.deepseek.com/v1".into(),
        api_key: "deepseek-key".into(),
        model_id: "deepseek-v4-pro".into(),
        reasoning: true,
        thinking_format: Some("deepseek-v4".into()),
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };

    let body =
        build_openai_stream_body(&resolved, &[], None, "off", &[], true).expect("body builds");

    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["thinking"]["type"], "disabled");
}

#[test]
fn build_openai_simple_body_deepseek_v4_includes_reasoning_content_in_messages() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://api.deepseek.com/v1".into(),
        api_key: "deepseek-key".into(),
        model_id: "deepseek-v4-pro".into(),
        reasoning: true,
        thinking_format: Some("deepseek-v4".into()),
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: Some("result".into()),
        images: None,
        thinking: Some("my reasoning".into()),
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"a.txt"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    }];

    let body = build_openai_simple_body(&resolved, &messages, None).expect("body builds");

    assert_eq!(
        body["messages"][0]["reasoning_content"].as_str(),
        Some("my reasoning")
    );
    // Simple body does not include thinking control fields
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("thinking").is_none());
}

#[test]
fn build_openai_simple_body_deepseek_v4_replays_missing_reasoning_tool_turn_as_text() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://api.deepseek.com/v1".into(),
        api_key: "deepseek-key".into(),
        model_id: "deepseek-v4-pro".into(),
        reasoning: true,
        thinking_format: Some("deepseek-v4".into()),
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![
        ChatMessage {
            role: "user".into(),
            content: Some("continue".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: r#"{"command":"dir"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("dir output".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            timestamp: None,
        },
    ];

    let body = build_openai_simple_body(&resolved, &messages, None).expect("body builds");
    let body_messages = body["messages"]
        .as_array()
        .expect("messages should serialize as an array");

    assert_eq!(body_messages.len(), 2);
    assert_eq!(body_messages[1]["role"], "assistant");
    assert!(body_messages[1].get("tool_calls").is_none());
    assert_eq!(
        body_messages[1]["reasoning_content"].as_str(),
        Some(
            "Historical tool turn summarized because the original DeepSeek response omitted reasoning_content."
        )
    );
    assert!(
        body_messages[1]["content"]
            .as_str()
            .expect("summary should be text")
            .contains("DeepSeek omitted reasoning_content")
    );
}

#[test]
fn build_openai_simple_body_non_deepseek_keeps_reasoningless_tool_turn_structured() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://example-openai-compatible.invalid/v1".into(),
        api_key: "api-key".into(),
        model_id: "gpt-4.1".into(),
        reasoning: true,
        thinking_format: None,
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![
        ChatMessage {
            role: "user".into(),
            content: Some("continue".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: r#"{"command":"dir"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("dir output".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("call_1".into()),
            timestamp: None,
        },
    ];

    let body = build_openai_simple_body(&resolved, &messages, None).expect("body builds");
    let body_messages = body["messages"]
        .as_array()
        .expect("messages should serialize as an array");

    assert_eq!(body_messages.len(), 3);
    assert_eq!(
        body_messages[1]["tool_calls"]
            .as_array()
            .expect("tool calls should stay structured")
            .len(),
        1
    );
    assert_eq!(body_messages[2]["role"], "tool");
}

#[test]
fn build_openai_simple_body_uses_null_tool_call_content_for_official_api() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: Provider::OpenAI.default_api_base().into(),
        api_key: "openai-key".into(),
        model_id: "gpt-4o-mini".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(128),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"README.md"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    }];

    let body = build_openai_simple_body(&resolved, &messages, None).expect("body builds");

    assert!(body["messages"][0]["content"].is_null());
}

#[test]
fn build_openai_simple_body_keeps_string_tool_call_content_for_compatible_api() {
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://vip.aipro.love/v1".into(),
        api_key: "openai-key".into(),
        model_id: "gpt-5.4".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(128),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"README.md"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    }];

    let body = build_openai_simple_body(&resolved, &messages, None).expect("body builds");

    assert_eq!(body["messages"][0]["content"], "");
}

#[test]
fn official_openai_api_base_requires_exact_hostname_match() {
    assert!(is_official_openai_api_base("https://api.openai.com/v1"));
    assert!(is_official_openai_api_base(
        "https://API.OPENAI.COM/v1/chat/completions"
    ));
    assert!(!is_official_openai_api_base(
        "https://proxy.example.com/openai/api.openai.com/v1"
    ));
    assert!(!is_official_openai_api_base(
        "https://api.openai.com.example.com/v1"
    ));
}

#[test]
fn auto_think_supported_for_openai_requires_official_or_explicit_format() {
    let compatible = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://vip.aipro.love/v1".into(),
        api_key: "openai-key".into(),
        model_id: "gpt-5.4".into(),
        reasoning: true,
        thinking_format: None,
        max_tokens: Some(128),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    assert!(!auto_think_supported(&compatible));

    let official = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: Provider::OpenAI.default_api_base().into(),
        api_key: "openai-key".into(),
        model_id: "gpt-5.4".into(),
        reasoning: true,
        thinking_format: None,
        max_tokens: Some(128),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    assert!(auto_think_supported(&official));

    let explicit = ResolvedModel {
        provider: Provider::OpenAI,
        api_base: "https://vip.aipro.love/v1".into(),
        api_key: "openai-key".into(),
        model_id: "gpt-5.4".into(),
        reasoning: true,
        thinking_format: Some("qwen".into()),
        max_tokens: Some(128),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    assert!(auto_think_supported(&explicit));
}

#[test]
fn openai_null_tool_call_content_allows_explicit_opt_in_for_custom_domain() {
    assert!(openai_prefers_null_tool_call_content_with_opt_in(
        "https://openai.internal.example/v1",
        true,
    ));
    assert!(!openai_prefers_null_tool_call_content_with_opt_in(
        "https://openai.internal.example/v1",
        false,
    ));
}

#[tokio::test]
async fn build_gemini_body_inlines_images_and_function_declarations() {
    let resolved = ResolvedModel {
        provider: Provider::Gemini,
        api_base: Provider::Gemini.default_api_base().into(),
        api_key: "gemini-key".into(),
        model_id: "gemini-2.5-flash".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(512),
        context_window: 1_000_000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("You are concise.".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("describe".into()),
            images: Some(vec![ImageAttachment {
                url: "memory://image.png".into(),
                s3_object_key: None,
                cache_path: None,
                data: Some(
                    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+/p9sAAAAASUVORK5CYII="
                        .into(),
                ),
            }]),
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    let workspace = unique_temp_dir("lingclaw-gemini-body");

    let body = build_gemini_body(&resolved, &messages, &workspace, None, &[], true, "off")
        .await
        .expect("gemini body should build");

    assert_eq!(
        body["systemInstruction"]["parts"][0]["text"],
        "You are concise."
    );
    assert_eq!(body["contents"][0]["role"], "user");
    assert_eq!(body["contents"][0]["parts"][0]["text"], "describe");
    assert_eq!(
        body["contents"][0]["parts"][1]["inlineData"]["mimeType"],
        "image/png"
    );
    assert_eq!(body["generationConfig"]["maxOutputTokens"], 512);
    assert!(body["tools"][0]["functionDeclarations"].is_array());
}

#[test]
fn convert_messages_to_gemini_preserves_function_call_id_and_thought_signature() {
    let messages = vec![
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "fc_1".into(),
                call_type: "function".into(),
                gemini_thought_signature: Some("sigA".into()),
                function: FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"README.md"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("contents".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("fc_1".into()),
            timestamp: None,
        },
    ];

    let (_, contents) = convert_messages_to_gemini(&messages, &std::collections::HashMap::new())
        .expect("Gemini history should convert");

    assert_eq!(contents.len(), 2);
    assert_eq!(contents[0]["role"], "model");
    assert_eq!(contents[0]["parts"][0]["functionCall"]["id"], "fc_1");
    assert_eq!(contents[0]["parts"][0]["functionCall"]["name"], "read_file");
    assert_eq!(contents[0]["parts"][0]["thoughtSignature"], "sigA");
    assert_eq!(contents[1]["role"], "user");
    assert_eq!(contents[1]["parts"][0]["functionResponse"]["id"], "fc_1");
    assert_eq!(
        contents[1]["parts"][0]["functionResponse"]["name"],
        "read_file"
    );
}

#[test]
fn convert_messages_to_gemini_omits_missing_thought_signature() {
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: None,
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: Some(vec![ToolCall {
            id: "fc_1".into(),
            call_type: "function".into(),
            gemini_thought_signature: None,
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"README.md"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    }];

    let (_, contents) = convert_messages_to_gemini(&messages, &std::collections::HashMap::new())
        .expect("Gemini history should convert");

    assert_eq!(contents[0]["parts"][0]["functionCall"]["id"], "fc_1");
    assert!(
        !contents[0]["parts"][0]
            .as_object()
            .unwrap()
            .contains_key("thoughtSignature")
    );
}

#[test]
fn convert_messages_to_gemini_groups_parallel_function_responses() {
    let messages = vec![
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "fc_1".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: Some("sig1".into()),
                    function: FunctionCall {
                        name: "read_file".into(),
                        arguments: r#"{"path":"a.txt"}"#.into(),
                    },
                },
                ToolCall {
                    id: "fc_2".into(),
                    call_type: "function".into(),
                    gemini_thought_signature: Some("sig2".into()),
                    function: FunctionCall {
                        name: "list_dir".into(),
                        arguments: r#"{"path":"."}"#.into(),
                    },
                },
            ]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("a".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("fc_1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("b".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("fc_2".into()),
            timestamp: None,
        },
    ];

    let (_, contents) = convert_messages_to_gemini(&messages, &std::collections::HashMap::new())
        .expect("Gemini history should convert");

    assert_eq!(contents.len(), 2);
    assert_eq!(contents[1]["role"], "user");
    let response_parts = contents[1]["parts"].as_array().unwrap();
    assert_eq!(response_parts.len(), 2);
    assert_eq!(response_parts[0]["functionResponse"]["id"], "fc_1");
    assert_eq!(response_parts[0]["functionResponse"]["name"], "read_file");
    assert_eq!(response_parts[1]["functionResponse"]["id"], "fc_2");
    assert_eq!(response_parts[1]["functionResponse"]["name"], "list_dir");
}

#[tokio::test]
async fn build_gemini_body_for_gemini3_includes_thinking_config() {
    let resolved = ResolvedModel {
        provider: Provider::Gemini,
        api_base: Provider::Gemini.default_api_base().into(),
        api_key: "gemini-key".into(),
        model_id: "gemini-3-flash-preview".into(),
        reasoning: true,
        thinking_format: None,
        max_tokens: Some(512),
        context_window: 1_000_000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hello".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let workspace = unique_temp_dir("lingclaw-gemini3-thinking");

    let body = build_gemini_body(&resolved, &messages, &workspace, None, &[], true, "low")
        .await
        .expect("gemini body should build");

    assert_eq!(body["generationConfig"]["maxOutputTokens"], 512);
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["includeThoughts"],
        true
    );
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "LOW"
    );
}

#[tokio::test]
async fn call_llm_stream_gemini_auto_enables_medium_thinking_config() {
    let response_body =
        "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"ok\"}]}}]}\n\n".to_string();
    let (api_base, request_rx, handle) =
        spawn_one_shot_http_server("text/event-stream", response_body);
    let http = reqwest::Client::new();
    let resolved = ResolvedModel {
        provider: Provider::Gemini,
        api_base,
        api_key: "gemini-key".into(),
        model_id: "gemini-3-flash-preview".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(512),
        context_window: 1_000_000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hello".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let workspace = unique_temp_dir("lingclaw-gemini-auto-thinking");
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let live_tx: LiveTx = tx;

    let response = call_llm_stream(
        &http,
        &resolved,
        &messages,
        &workspace,
        None,
        &live_tx,
        "auto",
        &[],
        0,
    )
    .await
    .expect("Gemini stream should complete");

    let request = request_rx.recv().expect("request should be captured");
    handle.join().expect("server thread should finish");
    let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
    assert_eq!(response.message.content.as_deref(), Some("ok"));
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["includeThoughts"],
        true
    );
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "MEDIUM"
    );
}

#[tokio::test]
async fn process_gemini_data_line_collects_text_tools_and_usage() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let live_tx: LiveTx = tx;
    let mut state = OpenAiStreamState {
        content_buf: String::new(),
        thinking_buf: String::new(),
        tool_calls: Vec::new(),
        input_tokens: None,
        output_tokens: None,
        client_gone: false,
        reasoning_started: false,
    };

    process_gemini_data_line(
        r#"{"candidates":[{"content":{"parts":[{"text":"hi"},{"functionCall":{"name":"read_file","args":{"path":"README.md"}}}]}}],"usageMetadata":{"promptTokenCount":7,"candidatesTokenCount":3,"thoughtsTokenCount":2}}"#,
        &live_tx,
        &mut state,
    )
    .await
    .expect("gemini stream line should parse");

    assert_eq!(state.content_buf, "hi");
    assert_eq!(state.input_tokens, Some(7));
    assert_eq!(state.output_tokens, Some(5));
    assert_eq!(state.tool_calls.len(), 1);
    assert_eq!(state.tool_calls[0].function.name, "read_file");
    assert_eq!(
        state.tool_calls[0].function.arguments,
        r#"{"path":"README.md"}"#
    );
    assert_eq!(rx.try_recv().unwrap()["type"], "delta");
}

#[tokio::test]
async fn process_gemini_data_line_collects_thought_summary_signature_and_function_call_id() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let live_tx: LiveTx = tx;
    let mut state = OpenAiStreamState {
        content_buf: String::new(),
        thinking_buf: String::new(),
        tool_calls: Vec::new(),
        input_tokens: None,
        output_tokens: None,
        client_gone: false,
        reasoning_started: false,
    };

    process_gemini_data_line(
        r#"{"candidates":[{"content":{"parts":[{"text":"plan","thought":true},{"functionCall":{"id":"fc_1","name":"read_file","args":{"path":"README.md"}},"thoughtSignature":"sigA"}]}}]}"#,
        &live_tx,
        &mut state,
    )
    .await
    .expect("gemini stream line should parse");

    assert_eq!(state.thinking_buf, "plan");
    assert_eq!(state.content_buf, "");
    assert_eq!(state.tool_calls.len(), 1);
    assert_eq!(state.tool_calls[0].id, "fc_1");
    assert_eq!(state.tool_calls[0].function.name, "read_file");
    assert_eq!(
        state.tool_calls[0].gemini_thought_signature.as_deref(),
        Some("sigA")
    );
    assert_eq!(rx.try_recv().unwrap()["type"], "thinking_start");
    assert_eq!(rx.try_recv().unwrap()["type"], "thinking_delta");
    assert_eq!(rx.try_recv().unwrap()["type"], "thinking_done");
}

#[tokio::test]
async fn call_llm_stream_gemini_closes_thinking_on_thought_only_stream_end() {
    let response_body = "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"plan\",\"thought\":true}]}}]}\n\n".to_string();
    let (api_base, request_rx, handle) =
        spawn_one_shot_http_server("text/event-stream", response_body);
    let http = reqwest::Client::new();
    let resolved = ResolvedModel {
        provider: Provider::Gemini,
        api_base,
        api_key: "gemini-key".into(),
        model_id: "gemini-3-flash-preview".into(),
        reasoning: true,
        thinking_format: None,
        max_tokens: Some(512),
        context_window: 1_000_000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("think".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let workspace = unique_temp_dir("lingclaw-gemini-thinking-end");
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let live_tx: LiveTx = tx;

    let response = call_llm_stream_gemini(
        &http,
        &resolved,
        &messages,
        &workspace,
        None,
        &live_tx,
        "low",
        &[],
        true,
        0,
    )
    .await
    .expect("Gemini stream should complete");

    let request = request_rx.recv().expect("request should be captured");
    assert!(request.request_line.contains(":streamGenerateContent"));
    handle.join().expect("server thread should finish");
    assert_eq!(response.message.thinking.as_deref(), Some("plan"));
    assert_eq!(rx.try_recv().unwrap()["type"], "thinking_start");
    assert_eq!(rx.try_recv().unwrap()["type"], "thinking_delta");
    assert_eq!(rx.try_recv().unwrap()["type"], "thinking_done");
}

#[tokio::test]
async fn process_gemini_data_line_rejects_blocked_prompt() {
    let (tx, _rx) = tokio::sync::mpsc::channel(16);
    let live_tx: LiveTx = tx;
    let mut state = OpenAiStreamState {
        content_buf: String::new(),
        thinking_buf: String::new(),
        tool_calls: Vec::new(),
        input_tokens: None,
        output_tokens: None,
        client_gone: false,
        reasoning_started: false,
    };

    let error = process_gemini_data_line(
        r#"{"promptFeedback":{"blockReason":"SAFETY"}}"#,
        &live_tx,
        &mut state,
    )
    .await
    .expect_err("blocked Gemini prompt should be surfaced");

    assert!(error.contains("SAFETY"));
}

#[tokio::test]
async fn build_gemini_body_rejects_invalid_cached_image_data() {
    let resolved = ResolvedModel {
        provider: Provider::Gemini,
        api_base: Provider::Gemini.default_api_base().into(),
        api_key: "gemini-key".into(),
        model_id: "gemini-2.5-flash".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(512),
        context_window: 1_000_000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("describe".into()),
        images: Some(vec![ImageAttachment {
            url: "memory://bad-image".into(),
            s3_object_key: None,
            cache_path: None,
            data: Some("not-base64".into()),
        }]),
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let workspace = unique_temp_dir("lingclaw-gemini-bad-image");

    let error = build_gemini_body(&resolved, &messages, &workspace, None, &[], true, "off")
        .await
        .expect_err("invalid Gemini image data should fail request construction");

    assert!(error.contains("not a supported PNG/JPEG payload"));
}

#[test]
fn call_llm_simple_gemini_sends_key_header_and_expected_path() {
    let response_body = r#"{"candidates":[{"content":{"parts":[{"text":"hello from gemini"}]}}],"usageMetadata":{"promptTokenCount":11,"candidatesTokenCount":5,"thoughtsTokenCount":2}}"#.to_string();
    let (api_base, request_rx, handle) =
        spawn_one_shot_http_server("application/json", response_body);
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    let http = reqwest::Client::new();
    let resolved = ResolvedModel {
        provider: Provider::Gemini,
        api_base: format!("{api_base}/v1beta"),
        api_key: "gemini-secret".into(),
        model_id: "gemini-2.5-flash".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(64),
        context_window: 1_000_000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hi".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let workspace = unique_temp_dir("lingclaw-call-gemini-simple");

    let response = runtime
        .block_on(async {
            call_llm_simple_with_usage(&http, &resolved, &messages, &workspace, None, 2).await
        })
        .expect("gemini simple call should succeed");

    let request = request_rx.recv().expect("captured request should exist");
    handle.join().expect("server thread should join");

    assert_eq!(response.content, "hello from gemini");
    assert_eq!(response.input_tokens, Some(11));
    assert_eq!(response.output_tokens, Some(7));
    assert_eq!(
        request.request_line,
        "POST /v1beta/models/gemini-2.5-flash:generateContent HTTP/1.1"
    );
    assert_eq!(
        request.headers.get("x-goog-api-key").map(String::as_str),
        Some("gemini-secret")
    );
    let body: serde_json::Value =
        serde_json::from_str(&request.body).expect("request body should be valid json");
    assert_eq!(body["contents"][0]["parts"][0]["text"], "hi");
    assert_eq!(body["generationConfig"]["maxOutputTokens"], 64);
    assert!(body.get("tools").is_none());
}

#[test]
fn call_llm_simple_openai_reports_raw_body_for_html_gateway_response() {
    let response_body = "<!doctype html><html><body>gateway landing page</body></html>".to_string();
    let (api_base, request_rx, handle) = spawn_one_shot_http_server("text/html", response_body);
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    let http = reqwest::Client::new();
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base,
        api_key: "openai-secret".into(),
        model_id: "gpt-4o-mini".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(16),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hi".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let workspace = unique_temp_dir("lingclaw-call-openai-html");

    let error = runtime
        .block_on(async { call_llm_simple(&http, &resolved, &messages, &workspace, None, 2).await })
        .expect_err("html response should surface a decode error with body context");

    let request = request_rx.recv().expect("captured request should exist");
    handle.join().expect("server thread should join");

    assert_eq!(request.request_line, "POST /chat/completions HTTP/1.1");
    assert!(error.contains("OpenAI decode error"));
    assert!(error.contains("<!doctype html>"));
    assert!(error.contains("gateway landing page"));
}

#[test]
fn call_llm_simple_openai_surfaces_json_error_envelope() {
    let response_body =
        r#"{"error":{"message":"bad key","type":"invalid_request_error"}}"#.to_string();
    let (api_base, request_rx, handle) =
        spawn_one_shot_http_server("application/json", response_body);
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    let http = reqwest::Client::new();
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base,
        api_key: "openai-secret".into(),
        model_id: "gpt-4o-mini".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(16),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hi".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let workspace = unique_temp_dir("lingclaw-call-openai-error-json");

    let error = runtime
        .block_on(async { call_llm_simple(&http, &resolved, &messages, &workspace, None, 2).await })
        .expect_err("error envelope should not be treated as a successful reply");

    let request = request_rx.recv().expect("captured request should exist");
    handle.join().expect("server thread should join");

    assert_eq!(request.request_line, "POST /chat/completions HTTP/1.1");
    assert!(error.contains("OpenAI API error"));
    assert!(error.contains("bad key"));
    assert!(error.contains("invalid_request_error"));
}

#[test]
fn call_llm_simple_ollama_sends_auth_and_expected_body() {
    let response_body = r#"{"message":{"content":"hello from ollama"}}"#.to_string();
    let (api_base, request_rx, handle) =
        spawn_one_shot_http_server("application/json", response_body);
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    let http = reqwest::Client::new();
    let resolved = ResolvedModel {
        provider: Provider::Ollama,
        api_base,
        api_key: "secret-key".into(),
        model_id: "qwen3".into(),
        reasoning: true,
        thinking_format: Some("ollama".into()),
        max_tokens: Some(64),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hi".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    let workspace = unique_temp_dir("lingclaw-call-simple");
    let content = runtime
        .block_on(async { call_llm_simple(&http, &resolved, &messages, &workspace, None, 2).await })
        .expect("ollama simple call should succeed");

    let request = request_rx.recv().expect("captured request should exist");
    handle.join().expect("server thread should join");

    assert_eq!(content, "hello from ollama");
    assert_eq!(request.request_line, "POST /api/chat HTTP/1.1");
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer secret-key")
    );

    let body: serde_json::Value =
        serde_json::from_str(&request.body).expect("request body should be valid json");
    assert_eq!(body["model"], "qwen3");
    assert_eq!(body["stream"], false);
    assert_eq!(body["options"]["num_predict"], 64);
    assert_eq!(body["options"]["num_ctx"], 128000);
    assert_eq!(body["messages"][0]["role"], "user");
    assert_eq!(body["messages"][0]["content"], "hi");
}

#[test]
fn call_llm_stream_ollama_parses_ndjson_end_to_end() {
    let response_body = concat!(
        r#"{"message":{"thinking":"step 1"},"done":false}"#,
        "\n",
        r#"{"message":{"content":"final answer","tool_calls":[{"id":"call_1","function":{"name":"read_file","arguments":{"path":"README.md"}}}]},"prompt_eval_count":17,"eval_count":5,"done":true}"#,
        "\n"
    )
    .to_string();
    let (api_base, request_rx, handle) =
        spawn_one_shot_http_server("application/x-ndjson", response_body);
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    let http = reqwest::Client::new();
    let resolved = ResolvedModel {
        provider: Provider::Ollama,
        api_base,
        api_key: "stream-key".into(),
        model_id: "qwen3".into(),
        reasoning: true,
        thinking_format: Some("ollama".into()),
        max_tokens: Some(128),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("inspect readme".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let live_tx: LiveTx = tx;

    let workspace = unique_temp_dir("lingclaw-call-stream");
    let response = runtime
        .block_on(async {
            call_llm_stream_ollama(
                &http,
                &resolved,
                &messages,
                &workspace,
                None,
                &live_tx,
                "high",
                &[],
                true,
                2,
            )
            .await
        })
        .expect("ollama stream call should succeed");

    let request = request_rx.recv().expect("captured request should exist");
    handle.join().expect("server thread should join");

    assert_eq!(request.request_line, "POST /api/chat HTTP/1.1");
    assert_eq!(
        request.headers.get("authorization").map(String::as_str),
        Some("Bearer stream-key")
    );
    let body: serde_json::Value =
        serde_json::from_str(&request.body).expect("request body should be valid json");
    assert_eq!(body["stream"], true);
    assert_eq!(body["think"], true);
    assert_eq!(body["options"]["num_predict"], 128);
    assert_eq!(body["options"]["num_ctx"], 128000);

    assert_eq!(response.message.content.as_deref(), Some("final answer"));
    assert_eq!(response.input_tokens, Some(17));
    assert_eq!(response.output_tokens, Some(5));
    let tool_calls = response
        .message
        .tool_calls
        .expect("stream response should keep tool calls");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[0].function.name, "read_file");
    assert_eq!(tool_calls[0].function.arguments, r#"{"path":"README.md"}"#);

    let mut event_types = Vec::new();
    while let Ok(event) = rx.try_recv() {
        let event_type = event["type"]
            .as_str()
            .expect("event type should be present")
            .to_string();
        event_types.push(event_type);
    }
    assert!(event_types.iter().any(|event| event == "thinking_start"));
    assert!(event_types.iter().any(|event| event == "thinking_delta"));
    assert!(event_types.iter().any(|event| event == "thinking_done"));
    assert!(event_types.iter().any(|event| event == "delta"));
}

#[test]
fn call_llm_stream_openai_reports_html_gateway_response() {
    let response_body = "<!doctype html><html><body>gateway landing page</body></html>".to_string();
    let (api_base, request_rx, handle) = spawn_one_shot_http_server("text/html", response_body);
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    let http = reqwest::Client::new();
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base,
        api_key: "stream-secret".into(),
        model_id: "gpt-4o-mini".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(32),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hi".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let live_tx: LiveTx = tx;

    let error = match runtime.block_on(async {
        call_llm_stream_openai(
            &http,
            &resolved,
            &messages,
            None,
            &live_tx,
            "medium",
            &[],
            true,
            2,
        )
        .await
    }) {
        Ok(_) => panic!("html response should fail before SSE parsing"),
        Err(error) => error,
    };

    let request = request_rx.recv().expect("captured request should exist");
    handle.join().expect("server thread should join");

    assert_eq!(request.request_line, "POST /chat/completions HTTP/1.1");
    assert!(error.contains("OpenAI stream error"));
    assert!(error.contains("text/html"));
    assert!(error.contains("gateway landing page"));
}

#[test]
fn call_llm_stream_openai_deepseek_multi_tool_stream_keeps_two_tool_calls_and_thinking() {
    let response_body = concat!(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"plan \"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"both files\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[",
        "{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}},",
        "{\"index\":1,\"id\":\"call_2\",\"function\":{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"Cargo.toml\\\"}\"}}",
        "]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n"
    )
    .to_string();
    let (api_base, request_rx, handle) =
        spawn_one_shot_http_server("text/event-stream", response_body);
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    let http = reqwest::Client::new();
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base,
        api_key: "deepseek-key".into(),
        model_id: "deepseek-v4-pro".into(),
        reasoning: true,
        thinking_format: Some("deepseek-v4".into()),
        max_tokens: Some(256),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("compare both files".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let live_tx: LiveTx = tx;

    let response = runtime
        .block_on(async {
            call_llm_stream_openai(
                &http,
                &resolved,
                &messages,
                None,
                &live_tx,
                "high",
                &[],
                true,
                2,
            )
            .await
        })
        .expect("deepseek multi-tool stream should succeed");

    let request = request_rx.recv().expect("captured request should exist");
    handle.join().expect("server thread should join");

    assert_eq!(request.request_line, "POST /chat/completions HTTP/1.1");
    let body: serde_json::Value =
        serde_json::from_str(&request.body).expect("request body should be valid json");
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["thinking"]["type"], "enabled");

    assert!(response.message.content.is_none());
    assert_eq!(
        response.message.thinking.as_deref(),
        Some("plan both files")
    );
    let tool_calls = response
        .message
        .tool_calls
        .expect("stream response should keep tool calls");
    assert_eq!(tool_calls.len(), 2);
    assert_eq!(tool_calls[0].id, "call_1");
    assert_eq!(tool_calls[0].function.name, "read_file");
    assert_eq!(tool_calls[0].function.arguments, r#"{"path":"README.md"}"#);
    assert_eq!(tool_calls[1].id, "call_2");
    assert_eq!(tool_calls[1].function.name, "read_file");
    assert_eq!(tool_calls[1].function.arguments, r#"{"path":"Cargo.toml"}"#);

    let mut event_types = Vec::new();
    while let Ok(event) = rx.try_recv() {
        let event_type = event["type"]
            .as_str()
            .expect("event type should be present")
            .to_string();
        event_types.push(event_type);
    }
    assert_eq!(
        event_types,
        vec![
            "thinking_start".to_string(),
            "thinking_delta".to_string(),
            "thinking_delta".to_string(),
            "thinking_done".to_string(),
        ]
    );
}

#[test]
fn call_llm_stream_openai_auto_skips_reasoning_effort_for_compatible_gateway() {
    let response_body =
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n".to_string();
    let (api_base, request_rx, handle) =
        spawn_one_shot_http_server("text/event-stream", response_body);
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    let http = reqwest::Client::new();
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base,
        api_key: "stream-secret".into(),
        model_id: "gpt-5.4".into(),
        reasoning: true,
        thinking_format: None,
        max_tokens: Some(32),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hi".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let live_tx: LiveTx = tx;
    let workspace = unique_temp_dir("lingclaw-openai-auto-compatible");

    let response = runtime
        .block_on(async {
            call_llm_stream(
                &http,
                &resolved,
                &messages,
                &workspace,
                None,
                &live_tx,
                "auto",
                &[],
                0,
            )
            .await
        })
        .expect("compatible gateway stream should succeed");

    let request = request_rx.recv().expect("captured request should exist");
    handle.join().expect("server thread should join");

    let body: serde_json::Value =
        serde_json::from_str(&request.body).expect("request body should be valid json");
    assert_eq!(response.message.content.as_deref(), Some("ok"));
    assert!(body.get("reasoning_effort").is_none());
    assert!(body.get("enable_thinking").is_none());
}

#[test]
fn call_llm_stream_openai_surfaces_json_error_envelope() {
    let response_body =
        r#"{"error":{"message":"bad key","type":"invalid_request_error"}}"#.to_string();
    let (api_base, request_rx, handle) =
        spawn_one_shot_http_server("application/json", response_body);
    let runtime = tokio::runtime::Runtime::new().expect("runtime should be created");
    let http = reqwest::Client::new();
    let resolved = ResolvedModel {
        provider: Provider::OpenAI,
        api_base,
        api_key: "stream-secret".into(),
        model_id: "gpt-4o-mini".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: Some(32),
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("hi".into()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    let live_tx: LiveTx = tx;

    let error = match runtime.block_on(async {
        call_llm_stream_openai(
            &http,
            &resolved,
            &messages,
            None,
            &live_tx,
            "medium",
            &[],
            true,
            2,
        )
        .await
    }) {
        Ok(_) => panic!("json error envelope should fail before SSE parsing"),
        Err(error) => error,
    };

    let request = request_rx.recv().expect("captured request should exist");
    handle.join().expect("server thread should join");

    assert_eq!(request.request_line, "POST /chat/completions HTTP/1.1");
    assert!(error.contains("OpenAI API error"));
    assert!(error.contains("bad key"));
    assert!(error.contains("invalid_request_error"));
}

#[test]
fn process_ollama_json_line_streams_thinking_content_and_tool_calls() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let live_tx: LiveTx = tx;

    let done = rt.block_on(async {
        let mut state = OpenAiStreamState {
            content_buf: String::new(),
            thinking_buf: String::new(),
            tool_calls: Vec::new(),
            input_tokens: None,
            output_tokens: None,
            client_gone: false,
            reasoning_started: false,
        };

        let _ = process_ollama_json_line(
            r#"{"message":{"thinking":"step 1"},"done":false}"#,
            &live_tx,
            &mut state,
        )
        .await;

        let done = process_ollama_json_line(
            r#"{"message":{"content":"answer","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"README.md"}}}]},"prompt_eval_count":12,"eval_count":3,"done":true}"#,
            &live_tx,
            &mut state,
        )
        .await;

        assert_eq!(state.content_buf, "answer");
        assert_eq!(state.input_tokens, Some(12));
        assert_eq!(state.output_tokens, Some(3));
        assert_eq!(state.tool_calls.len(), 1);
        assert_eq!(state.tool_calls[0].function.name, "read_file");
        assert_eq!(state.tool_calls[0].function.arguments, r#"{"path":"README.md"}"#);

        done
    });

    assert!(done);
    assert!(rx.try_recv().is_ok());
}

#[test]
fn anthropic_prompt_caching_is_enabled_for_official_api() {
    let resolved = ResolvedModel {
        provider: Provider::Anthropic,
        api_base: "https://api.anthropic.com".into(),
        api_key: "key".into(),
        model_id: "claude".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };

    assert!(anthropic_prompt_caching_enabled(&resolved));
}

#[test]
fn anthropic_prompt_caching_is_disabled_for_compatible_api_by_default() {
    let resolved = ResolvedModel {
        provider: Provider::Anthropic,
        api_base: "https://anthropic-compatible.example".into(),
        api_key: "key".into(),
        model_id: "claude".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: false,
    };

    assert!(!anthropic_prompt_caching_enabled(&resolved));
}

#[test]
fn anthropic_prompt_caching_can_be_forced_for_compatible_api() {
    let resolved = ResolvedModel {
        provider: Provider::Anthropic,
        api_base: "https://anthropic-compatible.example".into(),
        api_key: "key".into(),
        model_id: "claude".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: None,
        context_window: 128000,
        stream_include_usage: false,
        anthropic_prompt_caching: true,
    };

    assert!(anthropic_prompt_caching_enabled(&resolved));
}

// ── is_transient_llm_error ──────────────────────────────────────────────

#[test]
fn transient_error_detects_429() {
    assert!(is_transient_llm_error(
        "API 429 (after 3 attempts): rate limited"
    ));
}

#[test]
fn transient_error_detects_5xx() {
    assert!(is_transient_llm_error(
        "API 502 (after 3 attempts): bad gateway"
    ));
    assert!(is_transient_llm_error(
        "API 500 (after 3 attempts): internal"
    ));
    assert!(is_transient_llm_error(
        "API 503 (after 3 attempts): unavailable"
    ));
    assert!(is_transient_llm_error(
        "API 504 (after 3 attempts): timeout"
    ));
}

#[test]
fn transient_error_detects_http_error() {
    assert!(is_transient_llm_error("HTTP error: connection reset"));
    assert!(is_transient_llm_error("HTTP error: request timed out"));
}

#[test]
fn transient_error_detects_exhausted_retries() {
    assert!(is_transient_llm_error(
        "LLM request failed after all retries"
    ));
}

#[test]
fn transient_error_rejects_stream_errors() {
    assert!(!is_transient_llm_error(
        "stream error: connection reset by peer"
    ));
}

#[test]
fn transient_error_rejects_client_disconnected() {
    assert!(!is_transient_llm_error("Client disconnected"));
}

#[test]
fn transient_error_rejects_auth_errors() {
    assert!(!is_transient_llm_error("API 401: Unauthorized"));
    assert!(!is_transient_llm_error("API 403: Forbidden"));
}

#[test]
fn transient_error_rejects_bad_request() {
    assert!(!is_transient_llm_error("API 400: Bad Request"));
}

#[test]
fn transient_error_rejects_unrecognized() {
    assert!(!is_transient_llm_error("some random error"));
    assert!(!is_transient_llm_error(""));
}

// ── Image attachment conversion tests ──────────────────────────

#[test]
fn convert_messages_to_openai_user_with_images() {
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("describe this".into()),
        images: Some(vec![
            ImageAttachment {
                url: "https://example.com/a.png".into(),
                s3_object_key: None,
                cache_path: None,
                data: None,
            },
            ImageAttachment {
                url: "https://example.com/b.jpg".into(),
                s3_object_key: None,
                cache_path: None,
                data: None,
            },
        ]),
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let out = convert_messages_to_openai_with_options(&messages, false, None);
    assert_eq!(out.len(), 1);
    let content = out[0]["content"]
        .as_array()
        .expect("content should be array");
    assert_eq!(content.len(), 3);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "describe this");
    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(content[1]["image_url"]["url"], "https://example.com/a.png");
    assert_eq!(content[2]["type"], "image_url");
    assert_eq!(content[2]["image_url"]["url"], "https://example.com/b.jpg");
}

#[test]
fn convert_messages_to_openai_strips_images_when_tool_messages_present() {
    let messages = vec![
        ChatMessage {
            role: "user".into(),
            content: Some("describe this".into()),
            images: Some(vec![ImageAttachment {
                url: "https://example.com/a.png".into(),
                s3_object_key: None,
                cache_path: None,
                data: None,
            }]),
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: FunctionCall {
                    name: "exec".into(),
                    arguments: r#"{"cmd":"ls"}"#.into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("file1.txt".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
    ];
    let out = convert_messages_to_openai_with_options(&messages, false, None);
    assert_eq!(out.len(), 3);
    // When tool messages are present, user message content must be a plain
    // string — not an array with image_url — to avoid 400 InvalidParameter
    // from OpenAI-compatible providers.
    assert_eq!(out[0]["role"], "user");
    assert!(
        out[0]["content"].is_string(),
        "content should be plain string when tool messages exist"
    );
    assert_eq!(out[0]["content"].as_str(), Some("describe this"));
    assert!(!out[0].get("content").unwrap().is_array());
}

#[test]
fn convert_messages_to_anthropic_user_with_images() {
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("what is this?".into()),
        images: Some(vec![ImageAttachment {
            url: "https://example.com/photo.png".into(),
            s3_object_key: None,
            cache_path: None,
            data: None,
        }]),
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let (_, out) = convert_messages_to_anthropic(&messages);
    assert_eq!(out.len(), 1);
    let content = out[0]["content"]
        .as_array()
        .expect("content should be array");
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[0]["text"], "what is this?");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["source"]["type"], "url");
    assert_eq!(content[1]["source"]["url"], "https://example.com/photo.png");
}

#[test]
fn convert_messages_to_ollama_user_with_images() {
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("describe".into()),
        images: Some(vec![
            ImageAttachment {
                url: "https://example.com/x.png".into(),
                s3_object_key: None,
                cache_path: Some("C:/tmp/x.b64".into()),
                data: Some("aW1hZ2VfZGF0YV94".into()),
            },
            ImageAttachment {
                url: "https://example.com/y.jpg".into(),
                s3_object_key: None,
                cache_path: Some("C:/tmp/y.b64".into()),
                data: Some("aW1hZ2VfZGF0YV95".into()),
            },
        ]),
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    // Simulate pre-fetched base64 data (as the real flow would do).
    let mut images_b64 = std::collections::HashMap::new();
    images_b64.insert(
        "https://example.com/x.png".to_string(),
        "aW1hZ2VfZGF0YV94".to_string(),
    );
    images_b64.insert(
        "https://example.com/y.jpg".to_string(),
        "aW1hZ2VfZGF0YV95".to_string(),
    );
    let out = convert_messages_to_ollama(&messages, &images_b64);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0]["role"], "user");
    assert_eq!(out[0]["content"], "describe");
    let images = out[0]["images"].as_array().expect("images should be array");
    assert_eq!(images.len(), 2);
    assert_eq!(images[0], "aW1hZ2VfZGF0YV94");
    assert_eq!(images[1], "aW1hZ2VfZGF0YV95");
}

#[test]
fn convert_messages_to_ollama_user_with_images_missing_b64() {
    // When base64 map is empty, images should be omitted (unfetchable images are skipped).
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("describe".into()),
        images: Some(vec![ImageAttachment {
            url: "https://example.com/x.png".into(),
            s3_object_key: None,
            cache_path: None,
            data: None,
        }]),
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];
    let images_b64 = std::collections::HashMap::new();
    let out = convert_messages_to_ollama(&messages, &images_b64);
    assert_eq!(out.len(), 1);
    assert!(out[0].get("images").is_none());
}

#[tokio::test]
async fn fetch_images_base64_reads_persisted_cache_without_refetch() {
    let workspace = unique_temp_dir("lingclaw-image-cache");
    fs::create_dir_all(&workspace).expect("workspace should be created");
    let cached = "aW1hZ2VfY2FjaGVk";
    let cache_path =
        persist_image_base64_cache(&workspace, "https://example.com/cached.png", cached)
            .await
            .expect("cache should be persisted");

    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("describe".into()),
        images: Some(vec![ImageAttachment {
            url: "https://example.com/cached.png".into(),
            s3_object_key: None,
            cache_path: Some(cache_path),
            data: None,
        }]),
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    let images_b64 = fetch_images_base64(&messages, &workspace, None, false)
        .await
        .expect("cached image should load");
    assert_eq!(
        images_b64
            .get("https://example.com/cached.png")
            .map(String::as_str),
        Some(cached)
    );

    let _ = fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn fetch_images_base64_skips_uncached_historical_fetch_failures() {
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("describe".into()),
        images: Some(vec![ImageAttachment {
            url: "http://127.0.0.1/stale.png".into(),
            s3_object_key: None,
            cache_path: None,
            data: None,
        }]),
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    let workspace = unique_temp_dir("lingclaw-stale-image-cache");
    let images_b64 = fetch_images_base64(&messages, &workspace, None, false)
        .await
        .expect("stale historical images should be skipped, not fail the request");
    assert!(images_b64.is_empty());

    let error = fetch_images_base64(&messages, &workspace, None, true)
        .await
        .expect_err("strict image fetch should fail on missing data");
    assert!(error.contains("Failed to fetch image"));
}

#[tokio::test]
async fn fetch_images_base64_trusted_uploaded_urls_bypass_ssrf_on_cache_miss() {
    let (base_url, request_rx, handle) =
        spawn_one_shot_http_server("image/png", "historical-image-body".to_string());
    let cfg = S3Config {
        endpoint: format!("{base_url}/storage"),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "images/".into(),
        url_expiry_secs: 3600,
        lifecycle_days: 14,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("describe".into()),
        images: Some(vec![ImageAttachment {
            url: "https://expired.example.test/old.png".into(),
            s3_object_key: Some("images/2026/demo.png".into()),
            cache_path: None,
            data: None,
        }]),
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    let hydrated =
        materialize_image_urls(&messages, Some(&cfg)).expect("uploaded image should presign");
    let workspace = unique_temp_dir("lingclaw-trusted-history-image");
    let images_b64 = fetch_images_base64(&hydrated, &workspace, Some(&cfg), false)
        .await
        .expect("trusted uploaded image should load on cache miss");

    assert_eq!(
        images_b64.get(hydrated[0].images.as_ref().unwrap()[0].url.as_str()),
        Some(&"aGlzdG9yaWNhbC1pbWFnZS1ib2R5".to_string())
    );

    let request = request_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("trusted request should reach local gateway");
    assert!(
        request
            .request_line
            .starts_with("GET /storage/bucket/images/2026/demo.png?")
    );
    handle.join().expect("server thread should exit cleanly");
}

#[test]
fn fetch_single_image_base64_trusted_allows_localhost_s3_gateways() {
    let (base_url, _request_rx, handle) =
        spawn_one_shot_http_server("image/png", "trusted-image-body".to_string());
    let safe_http = build_image_fetch_client().expect("safe image client should build");
    let runtime = tokio::runtime::Runtime::new().expect("runtime should build");

    let result = runtime
        .block_on(async {
            fetch_single_image_base64_trusted(&format!("{base_url}/photo.png"), &safe_http).await
        })
        .expect("trusted localhost image fetch should bypass SSRF checks");

    assert_eq!(result, "dHJ1c3RlZC1pbWFnZS1ib2R5");
    handle.join().expect("server thread should exit cleanly");
}

#[test]
fn materialize_image_urls_refreshes_uploaded_s3_urls() {
    let cfg = S3Config {
        endpoint: "https://minio.example.test/storage".into(),
        region: "us-east-1".into(),
        bucket: "bucket".into(),
        access_key: "access-key".into(),
        secret_key: "secret-key".into(),
        prefix: "images/".into(),
        url_expiry_secs: 3600,
        lifecycle_days: 14,
    };
    let messages = vec![ChatMessage {
        role: "user".into(),
        content: Some("describe".into()),
        images: Some(vec![ImageAttachment {
            url: "https://expired.example.test/old.png".into(),
            s3_object_key: Some("images/2026/demo.png".into()),
            cache_path: None,
            data: None,
        }]),
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }];

    let hydrated =
        materialize_image_urls(&messages, Some(&cfg)).expect("s3 object key should presign");
    let url = hydrated[0].images.as_ref().unwrap()[0].url.as_str();

    assert!(url.starts_with("https://minio.example.test/storage/bucket/images/2026/demo.png?"));
    assert!(url.contains("X-Amz-Signature="));
}

#[test]
fn resolve_image_cache_path_accepts_session_cache_and_rejects_external_path() {
    let workspace = unique_temp_dir("lingclaw-image-cache-validate");
    let cache_dir = workspace.join(".image-cache");
    fs::create_dir_all(&cache_dir).expect("cache dir should be created");

    let valid_path = cache_dir.join("valid.b64");
    fs::write(&valid_path, "aW1hZ2U=").expect("valid cache file should be written");
    let resolved = resolve_image_cache_path(valid_path.to_str().expect("utf8 path"), &workspace)
        .expect("workspace cache should be accepted");
    assert_eq!(resolved, valid_path.canonicalize().expect("canonical path"));

    let relative = resolve_image_cache_path(".image-cache/valid.b64", &workspace)
        .expect("relative cache path should resolve inside workspace");
    assert_eq!(relative, valid_path.canonicalize().expect("canonical path"));

    let outside_workspace = unique_temp_dir("lingclaw-image-cache-external");
    let outside_cache_dir = outside_workspace.join(".image-cache");
    fs::create_dir_all(&outside_cache_dir).expect("external cache dir should be created");
    let outside_path = outside_cache_dir.join("external.b64");
    fs::write(&outside_path, "aW1hZ2U=").expect("external cache file should be written");
    assert!(
        resolve_image_cache_path(outside_path.to_str().expect("utf8 path"), &workspace).is_err()
    );

    let non_cache_file = workspace.join("secret.b64");
    fs::write(&non_cache_file, "aW1hZ2U=").expect("non-cache file should be written");
    assert!(
        resolve_image_cache_path(non_cache_file.to_str().expect("utf8 path"), &workspace).is_err()
    );

    let traversal_path = cache_dir.join("..").join("secret.b64");
    assert!(
        resolve_image_cache_path(traversal_path.to_str().expect("utf8 path"), &workspace).is_err()
    );

    let _ = fs::remove_dir_all(&workspace);
    let _ = fs::remove_dir_all(&outside_workspace);
}
