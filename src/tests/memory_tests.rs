use super::*;
use std::path::PathBuf;

#[test]
fn test_structured_memory_default() {
    let mem = StructuredMemory::default();
    assert!(mem.user_context.is_none());
    assert!(mem.facts.is_empty());
    assert_eq!(mem.updated_at, 0);
}

#[test]
fn test_save_and_load_structured_memory() {
    let dir = std::env::temp_dir().join("lingclaw_test_memory");
    let _ = std::fs::create_dir_all(&dir);

    let mem = StructuredMemory {
        user_context: Some("Prefers Rust".to_string()),
        facts: vec![MemoryFact {
            key: "lang".to_string(),
            value: "Rust".to_string(),
            recorded_at: 1000,
        }],
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
fn test_load_missing_returns_default() {
    let dir = PathBuf::from("/nonexistent/path/lingclaw_test_missing");
    let mem = load_structured_memory(&dir);
    assert!(mem.user_context.is_none());
    assert!(mem.facts.is_empty());
}

#[test]
fn test_format_memory_for_injection_empty() {
    let mem = StructuredMemory::default();
    assert!(format_memory_for_injection(&mem).is_none());
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
        updated_at: 100,
    };

    let result = format_memory_for_injection(&mem).unwrap();
    assert!(result.contains("Structured Memory"));
    assert!(result.contains("Likes concise code"));
    assert!(result.contains("preferred_language"));
    assert!(result.contains("Rust"));
    assert!(result.contains("LingClaw"));
}

#[test]
fn test_strip_json_fences() {
    assert_eq!(strip_json_fences("```json\n{\"a\":1}\n```"), "{\"a\":1}");
    assert_eq!(strip_json_fences("```\n{\"a\":1}\n```"), "{\"a\":1}");
    assert_eq!(strip_json_fences("{\"a\":1}"), "{\"a\":1}");
}

#[test]
fn test_build_conversation_excerpt() {
    let messages = vec![
        crate::ChatMessage {
            role: "system".into(),
            content: Some("system prompt".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "user".into(),
            content: Some("Hello".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "assistant".into(),
            content: Some("Hi there".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "tool".into(),
            content: Some("tool output".into()),
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
    let dir = std::env::temp_dir().join("lingclaw_test_mem_status_empty");
    let _ = std::fs::create_dir_all(&dir);

    let status = memory_status(&dir);
    assert!(status.contains("empty"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_memory_status_with_data() {
    let dir = std::env::temp_dir().join("lingclaw_test_mem_status_data");
    let _ = std::fs::create_dir_all(&dir);

    let mem = StructuredMemory {
        user_context: Some("Test user".to_string()),
        facts: vec![MemoryFact {
            key: "test".to_string(),
            value: "value".to_string(),
            recorded_at: 0,
        }],
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
    let dir = std::env::temp_dir().join("lingclaw_test_mem_status_utf8");
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "user".into(),
            content: Some("Hello".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "assistant".into(),
            content: Some("Real reply".into()),
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
    let dir = std::env::temp_dir().join("lingclaw_test_mem_debug_status");
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
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "user".into(),
            content: Some("Search for files".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        crate::ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(vec![crate::ToolCall {
                id: "tc1".into(),
                call_type: "function".into(),
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
            tool_calls: None,
            tool_call_id: Some("tc1".into()),
            timestamp: None,
        },
        crate::ChatMessage {
            role: "assistant".into(),
            content: Some("I found 3 files.".into()),
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
        updated_at: 2000,
    };

    let injected = format_memory_for_injection(&mem).unwrap();
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
        tool_calls: None,
        tool_call_id: Some("tc1".into()),
        timestamp: None,
    }];

    let excerpt = build_conversation_excerpt(&messages);
    assert!(excerpt.contains("[tool result:"));
    // Should be truncated, not include all 500 chars.
    assert!(excerpt.len() < 400);
}
