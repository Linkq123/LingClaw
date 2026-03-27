use super::*;

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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "assistant".into(),
            content: Some("hi".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "tool".into(),
            content: Some("result".into()),
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        ChatMessage {
            role: "unknown_role".into(),
            content: Some("skip me".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];
    let out = convert_messages_to_openai(&messages);
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
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
            function: FunctionCall {
                name: "exec".into(),
                arguments: r#"{"cmd":"ls"}"#.into(),
            },
        }]),
        tool_call_id: None,
        timestamp: None,
    }];
    let out = convert_messages_to_openai(&messages);
    assert_eq!(out.len(), 1);
    assert!(out[0]["tool_calls"].is_array());
    assert_eq!(out[0]["tool_calls"][0]["function"]["name"], "exec");
}

#[test]
fn convert_messages_to_anthropic_system_extraction() {
    let messages = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("system prompt".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some("hello".into()),
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
        tool_calls: Some(vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
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
}

#[test]
fn convert_messages_to_anthropic_empty_assistant_gets_placeholder() {
    let messages = vec![ChatMessage {
        role: "assistant".into(),
        content: None,
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
fn build_llm_response_empty_content_and_no_tools() {
    let resp = build_llm_response(String::new(), vec![], None, None).unwrap();
    assert!(resp.message.content.is_none());
    assert!(resp.message.tool_calls.is_none());
    assert_eq!(resp.message.role, "assistant");
}

#[test]
fn build_llm_response_with_content_and_tools() {
    let resp = build_llm_response(
        "thinking...".into(),
        vec![ToolCall {
            id: "tc1".into(),
            call_type: "function".into(),
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
fn process_openai_data_line_reports_done_marker() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let live_tx: LiveTx = tx;

    let done = rt.block_on(async {
        let mut state = OpenAiStreamState {
            content_buf: String::new(),
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
fn process_anthropic_sse_line_keeps_event_type_between_lines() {
    let rt = tokio::runtime::Runtime::new().expect("runtime should be created");
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let live_tx: LiveTx = tx;

    let content = rt.block_on(async {
        let mut state = AnthropicStreamState {
            current_event_type: String::new(),
            content_buf: String::new(),
            tool_calls: Vec::new(),
            input_tokens: None,
            output_tokens: None,
            block_tool_idx: HashMap::new(),
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
fn anthropic_prompt_caching_is_enabled_for_official_api() {
    let resolved = ResolvedModel {
        provider: Provider::Anthropic,
        api_base: "https://api.anthropic.com".into(),
        api_key: "key".into(),
        model_id: "claude".into(),
        reasoning: false,
        thinking_format: None,
        max_tokens: None,
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
        stream_include_usage: false,
        anthropic_prompt_caching: true,
    };

    assert!(anthropic_prompt_caching_enabled(&resolved));
}
