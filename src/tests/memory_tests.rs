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
    assert!(!excerpt.contains("tool output"));
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
