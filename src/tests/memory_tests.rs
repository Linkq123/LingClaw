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
