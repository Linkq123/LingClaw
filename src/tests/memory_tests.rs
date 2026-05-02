use super::*;
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex as AsyncMutex;

static NEXT_TEMP_DIR_ID: AtomicU64 = AtomicU64::new(1);

fn unique_temp_dir(label: &str) -> PathBuf {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let serial = NEXT_TEMP_DIR_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "lingclaw-{label}-{}-{suffix}-{serial}",
        std::process::id()
    ))
}

fn queue_test_config() -> Config {
    Config {
        api_key: "test-key".to_string(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: Default::default(),
        memory_model: None,
        reflection_model: None,
        context_model: None,
        provider: crate::Provider::OpenAI,
        openai_stream_include_usage: false,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: crate::DEFAULT_PORT,
        max_context_tokens: 32000,
        exec_timeout: Duration::from_secs(30),
        tool_timeout: Duration::from_secs(30),
        sub_agent_timeout: Duration::from_secs(300),
        max_llm_retries: 2,
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
        structured_memory: true,
        daily_reflection: false,
        s3: None,
        enable_state_digest: true,
    }
}

#[test]
fn test_structured_memory_default() {
    let mem = StructuredMemory::default();
    assert!(mem.user_context.is_none());
    assert!(mem.facts.is_empty());
    assert!(mem.lessons.is_empty());
    assert!(mem.open_loops.is_empty());
    assert!(mem.command_patterns.is_empty());
    assert!(mem.project_signals.is_empty());
    assert_eq!(mem.updated_at, 0);
}

#[test]
fn test_save_and_load_structured_memory() {
    let dir = unique_temp_dir("test-memory");
    let _ = std::fs::create_dir_all(&dir);

    let mem = StructuredMemory {
        user_context: Some("Prefers Rust".to_string()),
        facts: vec![MemoryFact {
            key: "lang".to_string(),
            value: "Rust".to_string(),
            recorded_at: 1000,
        }],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 1000,
    };

    save_structured_memory(&dir, &mem).unwrap();
    let loaded = load_structured_memory(&dir);

    assert_eq!(loaded.user_context.as_deref(), Some("Prefers Rust"));
    assert_eq!(loaded.facts.len(), 1);
    assert_eq!(loaded.facts[0].key, "lang");
    assert_eq!(loaded.facts[0].value, "Rust");

    // Cleanup
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_load_structured_memory_refreshes_cache_after_external_write() {
    let dir = unique_temp_dir("test-memory-cache-refresh");
    let _ = std::fs::create_dir_all(&dir);

    let original = StructuredMemory {
        user_context: Some("original".to_string()),
        facts: vec![MemoryFact {
            key: "lang".to_string(),
            value: "Rust".to_string(),
            recorded_at: 1000,
        }],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 1000,
    };
    save_structured_memory(&dir, &original).unwrap();
    let loaded = load_structured_memory(&dir);
    assert_eq!(loaded.user_context.as_deref(), Some("original"));

    std::thread::sleep(std::time::Duration::from_millis(1100));

    let updated = StructuredMemory {
        user_context: Some("updated".to_string()),
        facts: vec![MemoryFact {
            key: "lang".to_string(),
            value: "Go".to_string(),
            recorded_at: 2000,
        }],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 2000,
    };
    let path = dir.join("structured_memory.json");
    let data = serde_json::to_string_pretty(&updated).unwrap();
    std::fs::write(&path, data).unwrap();

    let refreshed = load_structured_memory(&dir);
    assert_eq!(refreshed.user_context.as_deref(), Some("updated"));
    assert_eq!(refreshed.facts[0].value, "Go");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_load_missing_returns_default() {
    let dir = PathBuf::from("/nonexistent/path/lingclaw_test_missing");
    let mem = load_structured_memory(&dir);
    assert!(mem.user_context.is_none());
    assert!(mem.facts.is_empty());
}

#[test]
fn test_format_memory_for_injection_empty() {
    let mem = StructuredMemory::default();
    assert!(format_memory_for_injection(&mem, None).is_none());
}

#[test]
fn test_format_memory_for_injection_with_facts() {
    let mem = StructuredMemory {
        user_context: Some("Likes concise code".to_string()),
        facts: vec![
            MemoryFact {
                key: "preferred_language".to_string(),
                value: "Rust".to_string(),
                recorded_at: 0,
            },
            MemoryFact {
                key: "project".to_string(),
                value: "LingClaw".to_string(),
                recorded_at: 0,
            },
        ],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 100,
    };

    let result = format_memory_for_injection(&mem, None).unwrap();
    assert!(result.contains("Structured Memory"));
    assert!(result.contains("Likes concise code"));
    assert!(result.contains("preferred_language"));
    assert!(result.contains("Rust"));
    assert!(result.contains("LingClaw"));
}

#[test]
fn test_strip_json_fences() {
    assert_eq!(
        crate::strip_json_fences("```json\n{\"a\":1}\n```"),
        "{\"a\":1}"
    );
    assert_eq!(
        crate::strip_json_fences("```JSON\n{\"a\":1}\n```"),
        "{\"a\":1}"
    );
    assert_eq!(
        crate::strip_json_fences("```Json\n{\"a\":1}\n```"),
        "{\"a\":1}"
    );
    assert_eq!(crate::strip_json_fences("```\n{\"a\":1}\n```"), "{\"a\":1}");
    assert_eq!(crate::strip_json_fences("{\"a\":1}"), "{\"a\":1}");
}

#[test]
fn test_build_conversation_excerpt() {
    let messages = vec![
        crate::ChatMessage {
            role: "system".into(),
            content: Some("system prompt".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "user".into(),
            content: Some("Hello".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "assistant".into(),
            content: Some("Hi there".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "tool".into(),
            content: Some("tool output".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
    ];

    let excerpt = build_conversation_excerpt(&messages);
    assert!(excerpt.contains("User: Hello"));
    assert!(excerpt.contains("Assistant: Hi there"));
    assert!(!excerpt.contains("system prompt"));
    // Tool results are now included as brief summaries for memory context.
    assert!(excerpt.contains("[tool result: tool output]"));
}

#[test]
fn test_memory_status_empty() {
    let dir = unique_temp_dir("test-mem-status-empty");
    let _ = std::fs::create_dir_all(&dir);

    let status = memory_status(&dir);
    assert!(status.contains("empty"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_memory_status_with_data() {
    let dir = unique_temp_dir("test-mem-status-data");
    let _ = std::fs::create_dir_all(&dir);

    let mem = StructuredMemory {
        user_context: Some("Test user".to_string()),
        facts: vec![MemoryFact {
            key: "test".to_string(),
            value: "value".to_string(),
            recorded_at: 0,
        }],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: crate::now_epoch(),
    };
    save_structured_memory(&dir, &mem).unwrap();

    let status = memory_status(&dir);
    assert!(status.contains("1 facts"));
    assert!(status.contains("test"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_memory_status_utf8_multibyte_no_panic() {
    let dir = unique_temp_dir("test-mem-status-utf8");
    let _ = std::fs::create_dir_all(&dir);

    // user_context with >100 bytes of Chinese chars (3 bytes each)
    let long_ctx = "你好世界".repeat(30); // 120 chars, 360 bytes
    let mem = StructuredMemory {
        user_context: Some(long_ctx),
        facts: vec![MemoryFact {
            key: "emoji".to_string(),
            value: "🦀".repeat(30), // 120 bytes of 4-byte chars
            recorded_at: 0,
        }],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: crate::now_epoch(),
    };
    save_structured_memory(&dir, &mem).unwrap();

    // Must not panic on multi-byte chars
    let status = memory_status(&dir);
    assert!(status.contains("emoji"));
    assert!(status.contains("…"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_build_conversation_excerpt_skips_auto_compress_summary() {
    let messages = vec![
        crate::ChatMessage {
            role: "assistant".into(),
            content: Some(
                "## Context Summary (auto-generated)\nPrevious conversation summary...".to_string(),
            ),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "user".into(),
            content: Some("Hello".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "assistant".into(),
            content: Some("Real reply".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let excerpt = build_conversation_excerpt(&messages);
    assert!(!excerpt.contains("Context Summary"));
    assert!(excerpt.contains("User: Hello"));
    assert!(excerpt.contains("Assistant: Real reply"));
}

#[test]
fn test_memory_runtime_status_unavailable_without_queue() {
    let status = memory_runtime_status(None);
    assert!(status.contains("Memory Updater"));
    assert!(status.contains("unavailable"));
}

#[tokio::test]
async fn test_memory_queue_replace_config_updates_runtime_snapshot() {
    let queue = MemoryUpdateQueue::spawn(
        queue_test_config(),
        Arc::new(AsyncMutex::new(HashMap::new())),
    );

    let mut new_config = queue_test_config();
    new_config.tool_timeout = Duration::from_secs(90);
    new_config.api_key = "new-key".to_string();
    queue.replace_config(new_config.clone());

    let snapshot = queue
        .config
        .lock()
        .expect("queue config lock should be available")
        .clone();
    assert_eq!(snapshot.tool_timeout, Duration::from_secs(90));
    assert_eq!(snapshot.api_key, "new-key");
}

#[tokio::test]
async fn test_memory_queue_shutdown_cancels_runtime_loop() {
    let queue = MemoryUpdateQueue::spawn(
        queue_test_config(),
        Arc::new(AsyncMutex::new(HashMap::new())),
    );

    queue.shutdown();

    assert!(queue.cancel.is_cancelled());
}

#[test]
fn test_format_queue_status_includes_counters_and_error() {
    let snapshot = MemoryQueueStatusSnapshot {
        state: "running".to_string(),
        enqueued: 3,
        replaced_during_debounce: 1,
        started: 2,
        succeeded: 1,
        failed: 1,
        timed_out: 0,
        last_model: Some("openai/gpt-4o-mini".to_string()),
        last_excerpt_chars: 321,
        last_duration_ms: 456,
        last_error: Some("parse LLM response: eof while parsing".to_string()),
        last_enqueued_at: crate::now_epoch(),
        last_started_at: crate::now_epoch(),
        last_finished_at: crate::now_epoch(),
        last_success_at: crate::now_epoch(),
        last_failure_at: crate::now_epoch(),
    };

    let status = format_queue_status(&snapshot);
    assert!(status.contains("State: running"));
    assert!(status.contains("enqueued 3"));
    assert!(status.contains("Debounce replacements: 1"));
    assert!(status.contains("openai/gpt-4o-mini"));
    assert!(status.contains("parse LLM response"));
}

#[tokio::test]
async fn test_memory_debug_status_includes_recent_audit_entries() {
    let dir = unique_temp_dir("test-mem-debug-status");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);

    append_memory_audit_record(
        &dir,
        &MemoryAuditRecord {
            timestamp: now_epoch_secs(),
            model: "openai/gpt-4o-mini".to_string(),
            status: "success".to_string(),
            excerpt_chars: 123,
            duration_ms: 77,
            facts_before: 1,
            facts_after: 2,
            entries_before: 1,
            entries_after: 2,
            had_user_context_before: false,
            had_user_context_after: true,
            changed: true,
            error: None,
        },
    )
    .await;

    let status = memory_debug_status(&dir, None);
    assert!(status.contains("Recent audit entries"));
    assert!(status.contains("success"));
    assert!(status.contains("facts 1 -> 2"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_build_conversation_excerpt_includes_tool_calls_and_results() {
    let messages = vec![
        crate::ChatMessage {
            role: "system".into(),
            content: Some("system prompt".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "user".into(),
            content: Some("Search for files".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "assistant".into(),
            content: None,
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: Some(vec![crate::ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
                gemini_thought_signature: None,
                function: crate::FunctionCall {
                    name: "search_files".into(),
                    arguments: "{}".into(),
                },
            }]),
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "tool".into(),
            content: Some("Found 3 matches in src/".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        crate::ChatMessage {
            role: "assistant".into(),
            content: Some("I found 3 files.".into()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
    ];

    let excerpt = build_conversation_excerpt(&messages);
    assert!(excerpt.contains("User: Search for files"));
    assert!(excerpt.contains("[tool: search_files]"));
    assert!(excerpt.contains("[tool result: Found 3 matches in src/]"));
    assert!(excerpt.contains("Assistant: I found 3 files."));
    assert!(!excerpt.contains("system prompt"));
}

#[test]
fn test_format_memory_for_injection_sorts_by_recency() {
    let mem = StructuredMemory {
        user_context: None,
        facts: vec![
            MemoryFact {
                key: "old".to_string(),
                value: "old value".to_string(),
                recorded_at: 1000,
            },
            MemoryFact {
                key: "new".to_string(),
                value: "new value".to_string(),
                recorded_at: 2000,
            },
        ],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 2000,
    };

    let injected = format_memory_for_injection(&mem, None).unwrap();
    let old_pos = injected.find("old value").unwrap();
    let new_pos = injected.find("new value").unwrap();
    // Newer fact should appear before older fact.
    assert!(new_pos < old_pos, "newer fact should be listed first");
}

#[test]
fn test_build_conversation_excerpt_truncates_long_tool_results() {
    let long_content = "x".repeat(500);
    let messages = vec![crate::ChatMessage {
        role: "tool".into(),
        content: Some(long_content),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: Some("tc1".into()),
        timestamp: None,
    }];

    let excerpt = build_conversation_excerpt(&messages);
    assert!(excerpt.contains("[tool result:"));
    // Should be truncated, not include all 500 chars.
    assert!(excerpt.len() < 400);
}

// ── Incremental memory merge tests ──────────────────────────────────────────

#[test]
fn test_merge_incremental_update_adds_new_fact() {
    let mut mem = StructuredMemory {
        user_context: Some("existing".into()),
        facts: vec![MemoryFact {
            key: "lang".into(),
            value: "Rust".into(),
            recorded_at: 100,
        }],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 100,
    };
    let raw: serde_json::Value = serde_json::from_str(
        r#"{"update_facts": [{"key": "editor", "value": "VS Code"}], "delete_facts": []}"#,
    )
    .unwrap();
    merge_llm_response_into_memory(&mut mem, &raw, 200);
    assert_eq!(mem.facts.len(), 2);
    assert_eq!(mem.facts[0].key, "lang");
    assert_eq!(mem.facts[0].recorded_at, 100); // unchanged
    assert_eq!(mem.facts[1].key, "editor");
    assert_eq!(mem.facts[1].value, "VS Code");
    assert_eq!(mem.facts[1].recorded_at, 200);
    // user_context unchanged (absent in response)
    assert_eq!(mem.user_context.as_deref(), Some("existing"));
}

#[test]
fn test_merge_incremental_update_modifies_existing() {
    let mut mem = StructuredMemory {
        user_context: None,
        facts: vec![MemoryFact {
            key: "lang".into(),
            value: "Python".into(),
            recorded_at: 100,
        }],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 100,
    };
    let raw: serde_json::Value = serde_json::from_str(
        r#"{"update_facts": [{"key": "lang", "value": "Rust"}], "delete_facts": []}"#,
    )
    .unwrap();
    merge_llm_response_into_memory(&mut mem, &raw, 200);
    assert_eq!(mem.facts.len(), 1);
    assert_eq!(mem.facts[0].value, "Rust");
    assert_eq!(mem.facts[0].recorded_at, 200); // updated timestamp
}

#[test]
fn test_merge_incremental_delete_removes_fact() {
    let mut mem = StructuredMemory {
        user_context: None,
        facts: vec![
            MemoryFact {
                key: "old".into(),
                value: "stale".into(),
                recorded_at: 50,
            },
            MemoryFact {
                key: "keep".into(),
                value: "important".into(),
                recorded_at: 100,
            },
        ],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 100,
    };
    let raw: serde_json::Value =
        serde_json::from_str(r#"{"update_facts": [], "delete_facts": ["old"]}"#).unwrap();
    merge_llm_response_into_memory(&mut mem, &raw, 200);
    assert_eq!(mem.facts.len(), 1);
    assert_eq!(mem.facts[0].key, "keep");
}

#[test]
fn test_merge_incremental_preserves_untouched_facts() {
    let mut mem = StructuredMemory {
        user_context: Some("ctx".into()),
        facts: vec![
            MemoryFact {
                key: "a".into(),
                value: "1".into(),
                recorded_at: 10,
            },
            MemoryFact {
                key: "b".into(),
                value: "2".into(),
                recorded_at: 20,
            },
            MemoryFact {
                key: "c".into(),
                value: "3".into(),
                recorded_at: 30,
            },
        ],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 30,
    };
    // Only update "b", leave "a" and "c" alone
    let raw: serde_json::Value = serde_json::from_str(
        r#"{"update_facts": [{"key": "b", "value": "updated"}], "delete_facts": []}"#,
    )
    .unwrap();
    merge_llm_response_into_memory(&mut mem, &raw, 200);
    assert_eq!(mem.facts.len(), 3);
    assert_eq!(mem.facts[0].value, "1"); // a unchanged
    assert_eq!(mem.facts[1].value, "updated"); // b updated
    assert_eq!(mem.facts[2].value, "3"); // c unchanged
}

#[test]
fn test_merge_legacy_full_replacement_still_works() {
    let mut mem = StructuredMemory {
        user_context: None,
        facts: vec![
            MemoryFact {
                key: "old".into(),
                value: "gone".into(),
                recorded_at: 50,
            },
            MemoryFact {
                key: "keep".into(),
                value: "same".into(),
                recorded_at: 100,
            },
        ],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 100,
    };
    // Legacy format: just "facts" key
    let raw: serde_json::Value = serde_json::from_str(
        r#"{"facts": [{"key": "keep", "value": "same"}, {"key": "new", "value": "added"}]}"#,
    )
    .unwrap();
    merge_llm_response_into_memory(&mut mem, &raw, 200);
    assert_eq!(mem.facts.len(), 2);
    assert_eq!(mem.facts[0].key, "keep");
    assert_eq!(mem.facts[0].recorded_at, 100); // preserved timestamp for same value
    assert_eq!(mem.facts[1].key, "new");
    assert_eq!(mem.facts[1].recorded_at, 200);
}

#[test]
fn test_merge_empty_response_preserves_memory() {
    let mut mem = StructuredMemory {
        user_context: Some("ctx".into()),
        facts: vec![MemoryFact {
            key: "lang".into(),
            value: "Rust".into(),
            recorded_at: 100,
        }],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 100,
    };
    let raw: serde_json::Value =
        serde_json::from_str(r#"{"update_facts": [], "delete_facts": []}"#).unwrap();
    merge_llm_response_into_memory(&mut mem, &raw, 200);
    assert_eq!(mem.facts.len(), 1);
    assert_eq!(mem.user_context.as_deref(), Some("ctx"));
}

#[test]
fn test_merge_same_value_is_noop() {
    let mut mem = StructuredMemory {
        user_context: None,
        facts: vec![MemoryFact {
            key: "lang".into(),
            value: "Rust".into(),
            recorded_at: 100,
        }],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 100,
    };
    let raw: serde_json::Value = serde_json::from_str(
        r#"{"update_facts": [{"key": "lang", "value": "Rust"}], "delete_facts": []}"#,
    )
    .unwrap();
    merge_llm_response_into_memory(&mut mem, &raw, 200);
    assert_eq!(mem.facts.len(), 1);
    assert_eq!(mem.facts[0].value, "Rust");
    assert_eq!(mem.facts[0].recorded_at, 100); // timestamp unchanged
}

#[test]
fn test_merge_normalizes_fact_keys_before_update() {
    let mut mem = StructuredMemory {
        user_context: None,
        facts: vec![MemoryFact {
            key: "preferred_language".into(),
            value: "Rust".into(),
            recorded_at: 100,
        }],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 100,
    };
    let raw: serde_json::Value = serde_json::from_str(
        r#"{"update_facts": [{"key": "Preferred Language", "value": "Go"}], "delete_facts": []}"#,
    )
    .unwrap();

    merge_llm_response_into_memory(&mut mem, &raw, 200);

    assert_eq!(mem.facts.len(), 1);
    assert_eq!(mem.facts[0].key, "preferred_language");
    assert_eq!(mem.facts[0].value, "Go");
}

#[test]
fn test_merge_normalizes_delete_fact_keys() {
    let mut mem = StructuredMemory {
        user_context: None,
        facts: vec![MemoryFact {
            key: "preferred_language".into(),
            value: "Rust".into(),
            recorded_at: 100,
        }],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 100,
    };
    let raw: serde_json::Value =
        serde_json::from_str(r#"{"update_facts": [], "delete_facts": ["Preferred Language"]}"#)
            .unwrap();

    merge_llm_response_into_memory(&mut mem, &raw, 200);

    assert!(mem.facts.is_empty());
}

#[test]
fn test_merge_dedupes_equivalent_fact_keys() {
    let mut mem = StructuredMemory {
        user_context: None,
        facts: vec![
            MemoryFact {
                key: "Preferred Language".into(),
                value: "Rust".into(),
                recorded_at: 100,
            },
            MemoryFact {
                key: "preferred_language".into(),
                value: "Go".into(),
                recorded_at: 200,
            },
        ],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 200,
    };

    merge_llm_response_into_memory(
        &mut mem,
        &serde_json::from_str(r#"{"update_facts": [], "delete_facts": []}"#).unwrap(),
        300,
    );

    assert_eq!(mem.facts.len(), 1);
    assert_eq!(mem.facts[0].key, "preferred_language");
    assert_eq!(mem.facts[0].value, "Go");
}

#[test]
fn test_format_memory_query_aware_sorting() {
    let mem = StructuredMemory {
        user_context: None,
        facts: vec![
            MemoryFact {
                key: "food".into(),
                value: "likes sushi".into(),
                recorded_at: 200, // newer but irrelevant
            },
            MemoryFact {
                key: "language".into(),
                value: "uses Rust primarily".into(),
                recorded_at: 100, // older but relevant
            },
        ],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 200,
    };
    // With query about Rust, the "language" fact should come first
    let result = format_memory_for_injection(&mem, Some("How do I compile Rust?")).unwrap();
    let lang_pos = result.find("language").unwrap();
    let food_pos = result.find("food").unwrap();
    assert!(
        lang_pos < food_pos,
        "relevant fact should be listed before irrelevant one"
    );

    // Without query, sorted by recency (food=200 first)
    let result_no_query = format_memory_for_injection(&mem, None).unwrap();
    let lang_pos2 = result_no_query.find("language").unwrap();
    let food_pos2 = result_no_query.find("food").unwrap();
    assert!(
        food_pos2 < lang_pos2,
        "without query, newer fact should be listed first"
    );
}

#[test]
fn test_tokenize_for_matching_handles_cjk() {
    // Pure CJK: each character becomes a separate token
    let tokens = crate::tokenize_for_matching("编程语言");
    assert_eq!(tokens, vec!["编", "程", "语", "言"]);

    // Mixed CJK + ASCII: ASCII words and CJK chars both emitted
    let tokens = crate::tokenize_for_matching("喜欢Rust语言");
    assert!(tokens.contains(&"rust".to_string()));
    assert!(tokens.contains(&"语".to_string()));
    assert!(tokens.contains(&"言".to_string()));
    assert!(tokens.contains(&"喜".to_string()));

    // Pure ASCII still works as before
    let tokens = crate::tokenize_for_matching("hello world");
    assert_eq!(tokens, vec!["hello", "world"]);

    // Short ASCII words (< 2 chars) are filtered
    let tokens = crate::tokenize_for_matching("I am OK");
    assert_eq!(tokens, vec!["am", "ok"]);

    // CJK punctuation should NOT become tokens
    let tokens = crate::tokenize_for_matching("你好。世界？");
    assert_eq!(tokens, vec!["你", "好", "世", "界"]);
}

#[test]
fn test_query_aware_sorting_with_cjk_query() {
    let mem = StructuredMemory {
        user_context: None,
        facts: vec![
            MemoryFact {
                key: "food".into(),
                value: "likes sushi".into(),
                recorded_at: 200,
            },
            MemoryFact {
                key: "language".into(),
                value: "使用Rust编程".into(),
                recorded_at: 100,
            },
        ],
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 200,
    };
    let result = format_memory_for_injection(&mem, Some("Rust编程")).unwrap();
    let lang_pos = result.find("language").unwrap();
    let food_pos = result.find("food").unwrap();
    assert!(
        lang_pos < food_pos,
        "CJK query should rank matching fact higher"
    );
}

#[test]
fn test_format_memory_for_injection_limits_irrelevant_facts_with_query() {
    let mut facts = vec![MemoryFact {
        key: "language".into(),
        value: "uses Rust primarily".into(),
        recorded_at: 100,
    }];
    for idx in 0..12 {
        facts.push(MemoryFact {
            key: format!("irrelevant_{idx}"),
            value: "miscellaneous note".into(),
            recorded_at: 2000 + idx,
        });
    }
    let mem = StructuredMemory {
        user_context: None,
        facts,
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 4000,
    };

    let result = format_memory_for_injection(&mem, Some("How do I compile Rust?")).unwrap();
    let fact_lines = result
        .lines()
        .filter(|line| line.starts_with("- **"))
        .count();

    assert!(result.contains("language"));
    assert!(
        fact_lines <= 4,
        "query-aware injection should keep relevant facts plus a small fallback set"
    );
}

#[test]
fn test_format_memory_for_injection_limits_recent_facts_without_query() {
    let facts = (0..12)
        .map(|idx| MemoryFact {
            key: format!("fact_{idx}"),
            value: format!("value_{idx}"),
            recorded_at: idx as u64,
        })
        .collect();
    let mem = StructuredMemory {
        user_context: None,
        facts,
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 12,
    };

    let result = format_memory_for_injection(&mem, None).unwrap();
    let fact_lines = result
        .lines()
        .filter(|line| line.starts_with("- **"))
        .count();

    assert_eq!(fact_lines, 8);
    assert!(result.contains("fact_11"));
    assert!(!result.contains("fact_0"));
}

#[test]
fn test_format_memory_for_injection_skips_fallback_when_relevant_facts_fill_cap() {
    let mut facts: Vec<MemoryFact> = (0..9)
        .map(|idx| MemoryFact {
            key: format!("language_{idx}"),
            value: "rust workspace".into(),
            recorded_at: 100 + idx as u64,
        })
        .collect();
    facts.extend((0..4).map(|idx| MemoryFact {
        key: format!("irrelevant_{idx}"),
        value: "miscellaneous note".into(),
        recorded_at: 1000 + idx as u64,
    }));
    let mem = StructuredMemory {
        user_context: None,
        facts,
        lessons: Vec::new(),
        open_loops: Vec::new(),
        command_patterns: Vec::new(),
        project_signals: Vec::new(),
        updated_at: 2000,
    };

    let result = format_memory_for_injection(&mem, Some("rust workspace")).unwrap();
    let fact_lines = result
        .lines()
        .filter(|line| line.starts_with("- **"))
        .count();

    assert_eq!(fact_lines, 8);
    assert!(!result.contains("irrelevant_"));
}

#[test]
fn test_load_structured_memory_backfills_new_typed_fields_from_legacy_json() {
    let dir = unique_temp_dir("test-memory-legacy-schema");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);

    let legacy = r#"{
  "user_context": "prefers rust",
  "facts": [{"key": "language", "value": "Rust", "recorded_at": 123}],
  "updated_at": 123
}"#;
    std::fs::write(dir.join("structured_memory.json"), legacy).unwrap();

    let loaded = load_structured_memory(&dir);
    assert_eq!(loaded.user_context.as_deref(), Some("prefers rust"));
    assert_eq!(loaded.facts.len(), 1);
    assert!(loaded.lessons.is_empty());
    assert!(loaded.open_loops.is_empty());
    assert!(loaded.command_patterns.is_empty());
    assert!(loaded.project_signals.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_merge_incremental_updates_typed_memory_categories() {
    let mut mem = StructuredMemory {
        lessons: vec![
            MemoryLesson {
                title: "Prefer cargo check".into(),
                when_to_apply: "before a full test pass".into(),
                recommendation: "Run cargo check first".into(),
                scope: "workflow".into(),
                confidence: MemoryConfidence::Medium,
                last_seen_at: 10,
            },
            MemoryLesson {
                title: "Old lesson".into(),
                when_to_apply: "legacy flow".into(),
                recommendation: "Ignore this".into(),
                scope: "general".into(),
                confidence: MemoryConfidence::Low,
                last_seen_at: 11,
            },
        ],
        open_loops: vec![
            OpenLoop {
                goal: "stabilize windows install".into(),
                blocker: "installer path unknown".into(),
                next_step: "inspect helper".into(),
                status: OpenLoopStatus::Open,
                updated_at: 10,
            },
            OpenLoop {
                goal: "remove flaky test".into(),
                blocker: "still reproduces".into(),
                next_step: "collect logs".into(),
                status: OpenLoopStatus::Open,
                updated_at: 11,
            },
        ],
        command_patterns: vec![
            CommandPattern {
                signature: "cargo test -- --nocapture".into(),
                purpose: "debug a failing test".into(),
                outcome: "verbose output".into(),
                confidence: MemoryConfidence::Medium,
                last_seen_at: 12,
            },
            CommandPattern {
                signature: "cargo clean".into(),
                purpose: "force a rebuild".into(),
                outcome: "slow but sometimes useful".into(),
                confidence: MemoryConfidence::Low,
                last_seen_at: 13,
            },
        ],
        project_signals: vec![
            ProjectSignal {
                key: "build_system".into(),
                value: "single crate".into(),
                recorded_at: 14,
            },
            ProjectSignal {
                key: "old_entry".into(),
                value: "remove me".into(),
                recorded_at: 15,
            },
        ],
        ..StructuredMemory::default()
    };

    let raw: serde_json::Value = serde_json::from_str(
        r#"{
          "update_lessons": [
            {
              "title": "Prefer cargo check",
              "when_to_apply": "before a full test pass",
              "recommendation": "Run cargo check before cargo test",
              "scope": "workflow",
              "confidence": "high"
            }
          ],
          "delete_lessons": ["Old lesson"],
          "update_open_loops": [
            {
              "goal": "stabilize windows install",
              "blocker": "warning still noisy",
              "next_step": "remove dead helper",
              "status": "in_progress"
            }
          ],
          "delete_open_loops": ["remove flaky test"],
          "update_command_patterns": [
            {
              "signature": "cargo test -- --nocapture",
              "purpose": "debug a failing test",
              "outcome": "captures detailed failure output",
              "confidence": "high"
            }
          ],
          "delete_command_patterns": ["cargo clean"],
          "update_project_signals": [
            {
              "key": "Build System",
              "value": "Cargo workspace"
            }
          ],
          "delete_project_signals": ["old_entry"]
        }"#,
    )
    .unwrap();

    merge_llm_response_into_memory(&mut mem, &raw, 200);

    assert_eq!(mem.lessons.len(), 1);
    assert_eq!(mem.lessons[0].title, "Prefer cargo check");
    assert_eq!(
        mem.lessons[0].recommendation,
        "Run cargo check before cargo test"
    );
    assert_eq!(mem.lessons[0].confidence, MemoryConfidence::High);

    assert_eq!(mem.open_loops.len(), 1);
    assert_eq!(mem.open_loops[0].goal, "stabilize windows install");
    assert_eq!(mem.open_loops[0].status, OpenLoopStatus::InProgress);
    assert_eq!(mem.open_loops[0].next_step, "remove dead helper");

    assert_eq!(mem.command_patterns.len(), 1);
    assert_eq!(
        mem.command_patterns[0].signature,
        "cargo test -- --nocapture"
    );
    assert_eq!(
        mem.command_patterns[0].outcome,
        "captures detailed failure output"
    );
    assert_eq!(mem.command_patterns[0].confidence, MemoryConfidence::High);

    assert_eq!(mem.project_signals.len(), 1);
    assert_eq!(mem.project_signals[0].key, "build_system");
    assert_eq!(mem.project_signals[0].value, "Cargo workspace");
}

#[test]
fn test_merge_incremental_updates_refresh_typed_memory_timestamps_on_same_content() {
    let mut mem = StructuredMemory {
        lessons: vec![MemoryLesson {
            title: "Prefer cargo check".into(),
            when_to_apply: "before a full test pass".into(),
            recommendation: "Run cargo check first".into(),
            scope: "workflow".into(),
            confidence: MemoryConfidence::Medium,
            last_seen_at: 10,
        }],
        open_loops: vec![OpenLoop {
            goal: "stabilize windows install".into(),
            blocker: "installer path unknown".into(),
            next_step: "inspect helper".into(),
            status: OpenLoopStatus::Open,
            updated_at: 20,
        }],
        command_patterns: vec![CommandPattern {
            signature: "cargo test -- --nocapture".into(),
            purpose: "debug a failing test".into(),
            outcome: "verbose output".into(),
            confidence: MemoryConfidence::Medium,
            last_seen_at: 30,
        }],
        project_signals: vec![ProjectSignal {
            key: "build_system".into(),
            value: "single crate".into(),
            recorded_at: 40,
        }],
        ..StructuredMemory::default()
    };

    let raw: serde_json::Value = serde_json::from_str(
        r#"{
          "update_lessons": [
            {
              "title": "Prefer cargo check",
              "when_to_apply": "before a full test pass",
              "recommendation": "Run cargo check first",
              "scope": "workflow",
              "confidence": "medium"
            }
          ],
          "update_open_loops": [
            {
              "goal": "stabilize windows install",
              "blocker": "installer path unknown",
              "next_step": "inspect helper",
              "status": "open"
            }
          ],
          "update_command_patterns": [
            {
              "signature": "cargo test -- --nocapture",
              "purpose": "debug a failing test",
              "outcome": "verbose output",
              "confidence": "medium"
            }
          ],
          "update_project_signals": [
            {
              "key": "build_system",
              "value": "single crate"
            }
          ]
        }"#,
    )
    .unwrap();

    merge_llm_response_into_memory(&mut mem, &raw, 200);

    assert_eq!(mem.lessons[0].last_seen_at, 200);
    assert_eq!(mem.open_loops[0].updated_at, 200);
    assert_eq!(mem.command_patterns[0].last_seen_at, 200);
    assert_eq!(mem.project_signals[0].recorded_at, 200);
}

#[test]
fn test_format_memory_for_injection_includes_typed_sections_in_order() {
    let mem = StructuredMemory {
        user_context: Some("prefers focused diffs".into()),
        facts: vec![MemoryFact {
            key: "language".into(),
            value: "Rust".into(),
            recorded_at: 50,
        }],
        lessons: vec![MemoryLesson {
            title: "Check first".into(),
            when_to_apply: "before a long test run".into(),
            recommendation: "Run cargo check first".into(),
            scope: "workflow".into(),
            confidence: MemoryConfidence::High,
            last_seen_at: 60,
        }],
        open_loops: vec![OpenLoop {
            goal: "stabilize install flow".into(),
            blocker: "helper warning still present".into(),
            next_step: "remove dead helper".into(),
            status: OpenLoopStatus::InProgress,
            updated_at: 70,
        }],
        command_patterns: vec![CommandPattern {
            signature: "cargo test -q".into(),
            purpose: "quick regression pass".into(),
            outcome: "fast smoke coverage".into(),
            confidence: MemoryConfidence::Medium,
            last_seen_at: 80,
        }],
        project_signals: vec![ProjectSignal {
            key: "entrypoint".into(),
            value: "src/main.rs".into(),
            recorded_at: 90,
        }],
        updated_at: 100,
    };

    let injected = format_memory_for_injection(&mem, None).unwrap();
    let open_loops_pos = injected.find("**Open loops:**").unwrap();
    let lessons_pos = injected.find("**Lessons:**").unwrap();
    let project_signals_pos = injected.find("**Project signals:**").unwrap();
    let command_patterns_pos = injected.find("**Command patterns:**").unwrap();
    let facts_pos = injected.find("**Remembered facts:**").unwrap();

    assert!(injected.contains("prefers focused diffs"));
    assert!(injected.contains("stabilize install flow"));
    assert!(injected.contains("Check first"));
    assert!(injected.contains("entrypoint"));
    assert!(injected.contains("cargo test -q"));
    assert!(injected.contains("language"));
    assert!(open_loops_pos < lessons_pos);
    assert!(lessons_pos < project_signals_pos);
    assert!(project_signals_pos < command_patterns_pos);
    assert!(command_patterns_pos < facts_pos);
}

#[test]
fn test_format_memory_for_injection_query_ranks_typed_items_by_relevance() {
    let mem = StructuredMemory {
        lessons: vec![
            MemoryLesson {
                title: "Rebase carefully".into(),
                when_to_apply: "before rewriting git history".into(),
                recommendation: "Create a backup branch first".into(),
                scope: "workflow".into(),
                confidence: MemoryConfidence::Medium,
                last_seen_at: 200,
            },
            MemoryLesson {
                title: "Rust test loop".into(),
                when_to_apply: "when validating a cargo workspace".into(),
                recommendation: "Run cargo test --workspace after cargo check".into(),
                scope: "repo".into(),
                confidence: MemoryConfidence::High,
                last_seen_at: 100,
            },
        ],
        project_signals: vec![
            ProjectSignal {
                key: "frontend_framework".into(),
                value: "solidjs".into(),
                recorded_at: 200,
            },
            ProjectSignal {
                key: "test_command".into(),
                value: "cargo test --workspace".into(),
                recorded_at: 100,
            },
        ],
        ..StructuredMemory::default()
    };

    let injected =
        format_memory_for_injection(&mem, Some("How do I test this cargo workspace?")).unwrap();
    let relevant_lesson_pos = injected.find("Rust test loop").unwrap();
    let irrelevant_lesson_pos = injected.find("Rebase carefully").unwrap();
    let relevant_signal_pos = injected.find("test_command").unwrap();
    let irrelevant_signal_pos = injected.find("frontend_framework").unwrap();

    assert!(relevant_lesson_pos < irrelevant_lesson_pos);
    assert!(relevant_signal_pos < irrelevant_signal_pos);
}

#[test]
fn test_retrieve_task_memory_uses_working_state_and_intent() {
    let mem = StructuredMemory {
        lessons: vec![
            MemoryLesson {
                title: "Rust test loop".into(),
                when_to_apply: "when validating a cargo workspace".into(),
                recommendation: "Run cargo check before cargo test --workspace".into(),
                scope: "repo".into(),
                confidence: MemoryConfidence::High,
                last_seen_at: 100,
            },
            MemoryLesson {
                title: "Git cleanup".into(),
                when_to_apply: "before force-pushing".into(),
                recommendation: "Create a backup branch first".into(),
                scope: "workflow".into(),
                confidence: MemoryConfidence::Medium,
                last_seen_at: 200,
            },
        ],
        open_loops: vec![
            OpenLoop {
                goal: "stabilize workspace tests".into(),
                blocker: "command choice is inconsistent".into(),
                next_step: "standardize on cargo test --workspace".into(),
                status: OpenLoopStatus::Open,
                updated_at: 150,
            },
            OpenLoop {
                goal: "refresh screenshots".into(),
                blocker: "missing latest assets".into(),
                next_step: "rerun the capture tool".into(),
                status: OpenLoopStatus::Open,
                updated_at: 250,
            },
        ],
        command_patterns: vec![
            CommandPattern {
                signature: "cargo test --workspace -- --nocapture".into(),
                purpose: "debug the full Rust workspace".into(),
                outcome: "shows verbose test failures".into(),
                confidence: MemoryConfidence::High,
                last_seen_at: 300,
            },
            CommandPattern {
                signature: "git push --force-with-lease".into(),
                purpose: "update a rewritten branch".into(),
                outcome: "safer than a plain force push".into(),
                confidence: MemoryConfidence::Medium,
                last_seen_at: 400,
            },
        ],
        project_signals: vec![
            ProjectSignal {
                key: "test_command".into(),
                value: "cargo test --workspace".into(),
                recorded_at: 120,
            },
            ProjectSignal {
                key: "frontend_framework".into(),
                value: "solidjs".into(),
                recorded_at: 220,
            },
        ],
        facts: vec![
            MemoryFact {
                key: "workspace_kind".into(),
                value: "cargo workspace".into(),
                recorded_at: 100,
            },
            MemoryFact {
                key: "deployment_env".into(),
                value: "staging".into(),
                recorded_at: 200,
            },
        ],
        ..StructuredMemory::default()
    };

    let mut state = crate::agent::WorkingState::default();
    state.seed_from_query(Some("zzzxxyyqq unreachabletoken"));
    state
        .open_questions
        .push("Which workspace test command should I use?".into());

    let retrieved = retrieve_task_memory(&mem, Some("zzzxxyyqq unreachabletoken"), Some(&state));

    assert_eq!(
        retrieved.command_patterns[0].signature,
        "cargo test --workspace -- --nocapture"
    );
    assert_eq!(retrieved.project_signals[0].key, "test_command");
    assert_eq!(retrieved.open_loops[0].goal, "stabilize workspace tests");
    assert!(
        retrieved
            .lessons
            .iter()
            .any(|lesson| lesson.title == "Rust test loop")
    );
    assert_eq!(retrieved.facts[0].key, "workspace_kind");
}

#[test]
fn test_retrieve_task_memory_uses_completed_steps_and_evidence_from_state() {
    let mem = StructuredMemory {
        lessons: vec![
            MemoryLesson {
                title: "Entrypoint wiring".into(),
                when_to_apply: "when tracing startup flow".into(),
                recommendation: "Inspect src/main.rs first".into(),
                scope: "repo".into(),
                confidence: MemoryConfidence::High,
                last_seen_at: 100,
            },
            MemoryLesson {
                title: "Timeout source".into(),
                when_to_apply: "when a timeout value appears in src/runtime.rs".into(),
                recommendation: "src/runtime.rs owns timeout_ms".into(),
                scope: "repo".into(),
                confidence: MemoryConfidence::High,
                last_seen_at: 200,
            },
        ],
        ..StructuredMemory::default()
    };

    let mut state = crate::agent::WorkingState::default();
    state.seed_from_query(Some("inspect the entrypoint wiring"));
    state
        .completed_steps
        .push("read_file succeeded: read `src/runtime.rs` in 4ms (call c1).".into());
    state.evidence.push(crate::agent::EvidenceItem {
        claim: "Observed file content: timeout_ms = 45".into(),
        source_tool: "read_file".into(),
        source_ref: "src/runtime.rs".into(),
        confidence: crate::agent::EvidenceConfidence::High,
    });

    let retrieved = retrieve_task_memory(&mem, Some("inspect the entrypoint wiring"), Some(&state));

    assert!(
        retrieved
            .lessons
            .iter()
            .any(|lesson| lesson.title == "Timeout source")
    );
}

#[test]
fn test_build_task_memory_query_keeps_blockers_ahead_of_long_evidence() {
    let mut state = crate::agent::WorkingState::default();
    state.seed_from_query(Some("inspect the runtime loop"));
    state.completed_steps.push(format!(
        "read_file succeeded: {}",
        "src/runtime_loop.rs timeout observation ".repeat(12)
    ));
    state.completed_steps.push(format!(
        "search_code succeeded: {}",
        "runtime_loop cancellation trace ".repeat(12)
    ));
    state.evidence.push(crate::agent::EvidenceItem {
        claim: format!(
            "Observed timeout-related context {}",
            "runtime_loop timeout path ".repeat(12)
        ),
        source_tool: "read_file".into(),
        source_ref: "src/runtime_loop.rs".into(),
        confidence: crate::agent::EvidenceConfidence::High,
    });
    state
        .open_questions
        .push("Which path still triggers blockerterm after the refactor?".into());
    state.uncertainties.push(crate::agent::UncertaintyItem {
        topic: "blockerterm".into(),
        reason: "still unresolved after the first runtime probe".into(),
        blocking: true,
    });
    state
        .next_actions
        .push("Reproduce blockerterm with a smaller runtime command.".into());

    let query = build_task_memory_query(Some("inspect the runtime loop"), Some(&state))
        .expect("task memory query should be built");

    assert!(query.contains("blockerterm"));
}

#[test]
fn test_build_task_memory_query_prefers_latest_blockers() {
    let mut state = crate::agent::WorkingState::default();
    state.seed_from_query(Some("continue the investigation"));
    state
        .open_questions
        .push("oldquestionalpha needs another pass".into());
    state
        .open_questions
        .push("newquestionomega is now the main blocker".into());
    state.uncertainties.push(crate::agent::UncertaintyItem {
        topic: "olduncertaintyalpha".into(),
        reason: "first blocker is no longer the active one".into(),
        blocking: true,
    });
    state.uncertainties.push(crate::agent::UncertaintyItem {
        topic: "newuncertaintyomega".into(),
        reason: "latest blocker still needs resolution".into(),
        blocking: true,
    });
    state
        .next_actions
        .push("retry oldactionalpha after the initial probe".into());
    state
        .next_actions
        .push("focus on newactionomega before revisiting older work".into());

    let query = build_task_memory_query(Some("continue the investigation"), Some(&state))
        .expect("task memory query should be built");

    assert!(query.contains("newquestionomega"));
    assert!(query.contains("newuncertaintyomega"));
    assert!(query.contains("newactionomega"));
}

#[test]
fn test_format_task_memory_for_prompt_renders_relevant_sections() {
    let selected = RetrievedTaskMemory {
        open_loops: vec![OpenLoop {
            goal: "stabilize install flow".into(),
            blocker: "warning still appears".into(),
            next_step: "remove the stale helper".into(),
            status: OpenLoopStatus::InProgress,
            updated_at: 10,
        }],
        lessons: vec![MemoryLesson {
            title: "Check first".into(),
            when_to_apply: "before a long test run".into(),
            recommendation: "Run cargo check first".into(),
            scope: "workflow".into(),
            confidence: MemoryConfidence::High,
            last_seen_at: 20,
        }],
        project_signals: vec![ProjectSignal {
            key: "entrypoint".into(),
            value: "src/main.rs".into(),
            recorded_at: 30,
        }],
        command_patterns: vec![CommandPattern {
            signature: "cargo test -q".into(),
            purpose: "quick regression pass".into(),
            outcome: "fast smoke coverage".into(),
            confidence: MemoryConfidence::Medium,
            last_seen_at: 40,
        }],
        facts: vec![MemoryFact {
            key: "language".into(),
            value: "Rust".into(),
            recorded_at: 50,
        }],
    };

    let rendered =
        format_task_memory_for_prompt(&selected, crate::agent::TaskIntent::Change).unwrap();

    assert!(rendered.starts_with("## Relevant Past Experience"));
    assert!(rendered.contains("Open loops to revisit"));
    assert!(rendered.contains("Relevant lessons"));
    assert!(rendered.contains("Project signals"));
    assert!(rendered.contains("Command patterns"));
    assert!(rendered.contains("Relevant facts"));
    assert!(rendered.contains("cargo check first"));
}

#[test]
fn test_format_task_tool_hints_for_prompt_reuses_commands_and_anchors() {
    let selected = RetrievedTaskMemory {
        open_loops: vec![OpenLoop {
            goal: "stabilize install flow".into(),
            blocker: "warning still appears".into(),
            next_step: "remove the stale helper".into(),
            status: OpenLoopStatus::Open,
            updated_at: 10,
        }],
        command_patterns: vec![CommandPattern {
            signature: "cargo test --workspace".into(),
            purpose: "validate the Rust workspace".into(),
            outcome: "full regression signal".into(),
            confidence: MemoryConfidence::High,
            last_seen_at: 20,
        }],
        project_signals: vec![ProjectSignal {
            key: "entrypoint".into(),
            value: "src/main.rs".into(),
            recorded_at: 30,
        }],
        ..RetrievedTaskMemory::default()
    };

    let rendered =
        format_task_tool_hints_for_prompt(&selected, crate::agent::TaskIntent::Change).unwrap();

    assert!(rendered.starts_with("## Tool Hints"));
    assert!(rendered.contains("Prefer `exec`"));
    assert!(rendered.contains("cargo test --workspace"));
    assert!(rendered.contains("Prefer `read_file` or `search_files`"));
    assert!(rendered.contains("src/main.rs"));
}

#[test]
fn test_task_tool_ranking_context_prefers_exec_and_file_tools() {
    let selected = RetrievedTaskMemory {
        open_loops: vec![OpenLoop {
            goal: "stabilize install flow".into(),
            blocker: "warning still appears".into(),
            next_step: "remove the stale helper".into(),
            status: OpenLoopStatus::Open,
            updated_at: 10,
        }],
        command_patterns: vec![CommandPattern {
            signature: "cargo test --workspace".into(),
            purpose: "validate the Rust workspace".into(),
            outcome: "full regression signal".into(),
            confidence: MemoryConfidence::High,
            last_seen_at: 20,
        }],
        project_signals: vec![ProjectSignal {
            key: "entrypoint".into(),
            value: "src/main.rs".into(),
            recorded_at: 30,
        }],
        ..RetrievedTaskMemory::default()
    };

    let ranking = task_tool_ranking_context(&selected, crate::agent::TaskIntent::Change);

    assert!(ranking.preferred_tools.contains(&"exec".to_string()));
    assert!(ranking.preferred_tools.contains(&"read_file".to_string()));
    assert!(
        ranking
            .preferred_tools
            .contains(&"search_files".to_string())
    );
    assert!(ranking.preferred_tools.contains(&"think".to_string()));
}

#[test]
fn test_task_memory_resolution_anchors_collects_concrete_paths_commands_and_urls() {
    let selected = RetrievedTaskMemory {
        open_loops: vec![OpenLoop {
            goal: "stabilize install flow".into(),
            blocker: "warning still appears".into(),
            next_step: "inspect Cargo.toml before retrying".into(),
            status: OpenLoopStatus::Open,
            updated_at: 10,
        }],
        command_patterns: vec![CommandPattern {
            signature: "cargo test --workspace".into(),
            purpose: "validate the Rust workspace".into(),
            outcome: "full regression signal".into(),
            confidence: MemoryConfidence::High,
            last_seen_at: 20,
        }],
        project_signals: vec![ProjectSignal {
            key: "entrypoint".into(),
            value: "src/main.rs".into(),
            recorded_at: 30,
        }],
        facts: vec![MemoryFact {
            key: "docs".into(),
            value: "https://example.com/spec".into(),
            recorded_at: 40,
        }],
        ..RetrievedTaskMemory::default()
    };

    let anchors = task_memory_resolution_anchors(&selected);

    assert!(
        anchors
            .iter()
            .any(|anchor| anchor == "cargo test --workspace")
    );
    assert!(anchors.iter().any(|anchor| anchor == "src/main.rs"));
    assert!(anchors.iter().any(|anchor| anchor == "Cargo.toml"));
    assert!(
        anchors
            .iter()
            .any(|anchor| anchor == "https://example.com/spec")
    );
}

#[test]
fn test_task_memory_next_actions_prefers_open_loops_and_lessons() {
    let selected = RetrievedTaskMemory {
        open_loops: vec![OpenLoop {
            goal: "windows install flow".into(),
            blocker: "helper warning remains".into(),
            next_step: "remove the stale helper".into(),
            status: OpenLoopStatus::Open,
            updated_at: 10,
        }],
        lessons: vec![MemoryLesson {
            title: "Rust test loop".into(),
            when_to_apply: "before a full workspace pass".into(),
            recommendation: "Run cargo check before cargo test --workspace".into(),
            scope: "repo".into(),
            confidence: MemoryConfidence::High,
            last_seen_at: 20,
        }],
        command_patterns: vec![CommandPattern {
            signature: "cargo test --workspace".into(),
            purpose: "validate the full Rust workspace".into(),
            outcome: "full regression signal".into(),
            confidence: MemoryConfidence::High,
            last_seen_at: 30,
        }],
        ..RetrievedTaskMemory::default()
    };

    let actions = task_memory_next_actions(&selected, crate::agent::TaskIntent::Execute);

    assert!(actions[0].contains("windows install flow"));
    assert!(
        actions
            .iter()
            .any(|action| action.contains("cargo check before cargo test --workspace"))
    );
    assert!(
        actions
            .iter()
            .any(|action| action.contains("cargo test --workspace"))
    );
}

#[test]
fn test_retrieve_task_memory_skips_irrelevant_recent_entries_when_query_misses() {
    let mem = StructuredMemory {
        lessons: vec![MemoryLesson {
            title: "Git cleanup".into(),
            when_to_apply: "before force-pushing".into(),
            recommendation: "Create a backup branch first".into(),
            scope: "workflow".into(),
            confidence: MemoryConfidence::Medium,
            last_seen_at: 200,
        }],
        command_patterns: vec![CommandPattern {
            signature: "git push --force-with-lease".into(),
            purpose: "update a rewritten branch".into(),
            outcome: "safer than a plain force push".into(),
            confidence: MemoryConfidence::High,
            last_seen_at: 300,
        }],
        project_signals: vec![ProjectSignal {
            key: "frontend_framework".into(),
            value: "solidjs".into(),
            recorded_at: 400,
        }],
        facts: vec![MemoryFact {
            key: "deployment_env".into(),
            value: "staging".into(),
            recorded_at: 500,
        }],
        ..StructuredMemory::default()
    };

    let mut state = crate::agent::WorkingState::default();
    state.seed_from_query(Some("zzzxxyyqq unreachabletoken"));
    let query_tokens = crate::tokenize_for_matching("zzzxxyyqq unreachabletoken");
    assert_eq!(
        lesson_relevance_score(&mem.lessons[0], &query_tokens, "zzzxxyyqq unreachabletoken"),
        0
    );

    let retrieved = retrieve_task_memory(&mem, Some("zzzxxyyqq unreachabletoken"), Some(&state));

    assert!(retrieved.lessons.is_empty());
    assert!(retrieved.command_patterns.is_empty());
    assert!(retrieved.project_signals.is_empty());
    assert!(retrieved.facts.is_empty());
}
