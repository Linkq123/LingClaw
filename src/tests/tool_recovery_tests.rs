use super::*;

#[test]
fn retry_identity_only_relaxes_the_read_window() {
    let key = |name, args| canonical_tool_retry_key(name, args);
    assert_eq!(
        key("read_file", r#"{"path":"src/main.rs","end_line":0}"#),
        key("read_file", r#"{"start_line":1,"path":"src/main.rs"}"#)
    );
    assert_ne!(
        key("mcp__server__Read", "{}"),
        key("mcp__server__read", "{}")
    );
    assert_ne!(
        key("read_file", r#"{"path":"a\\b"}"#),
        key("read_file", r#"{"path":"a/b"}"#)
    );
    assert_ne!(
        key("read_file", r#"{"path":"a"}"#),
        key("read_file", r#"{"path":"b"}"#)
    );
    assert_ne!(
        key("session_control", r#"{"action":"stop","session":"a"}"#),
        key("session_control", r#"{"action":"dispatch","session":"a"}"#)
    );
    assert_ne!(
        key("search", r#"{"path":"a","query":"one"}"#),
        key("search", r#"{"path":"a","query":"two"}"#)
    );
    assert_ne!(
        key("task", r#"{"agent":"explore","prompt":"one"}"#),
        key("task", r#"{"agent":"explore","prompt":"two"}"#)
    );
    assert_eq!(
        key("task", r#"{"agent":"explore","prompt":"one"}"#),
        key("task", r#"{"prompt":"one","agent":"explore"}"#)
    );
}
