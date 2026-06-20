use super::*;

static TEST_CONTROL_REGISTRY_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

fn control_registry_test_guard() -> std::sync::MutexGuard<'static, ()> {
    TEST_CONTROL_REGISTRY_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn clear_direct_runs_for_test() {
    DIRECT_RUNS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

fn clear_group_run_controls_for_test() {
    GROUP_RUN_CONTROLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

fn test_direct_control() -> DelegatedRunControl {
    DelegatedRunControl {
        cancel: CancellationToken::new(),
        stop_requested: Arc::new(AtomicBool::new(false)),
    }
}

fn test_message(role: &str, content: &str) -> ChatMessage {
    ChatMessage {
        role: role.to_string(),
        content: Some(content.to_string()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: None,
    }
}

fn test_group_run(status: &str) -> GroupRun {
    GroupRun {
        id: "grun-test".to_string(),
        group_id: "group-test".to_string(),
        session_id: "worker-a".to_string(),
        status: status.to_string(),
        prompt: "inspect".to_string(),
        result_excerpt: None,
        error: None,
        created_at: 1,
        updated_at: 1,
        completed_at: None,
    }
}

fn test_session_with_workspace(id: &str, name: &str, workspace: std::path::PathBuf) -> Session {
    Session {
        id: id.to_string(),
        name: name.to_string(),
        messages: Vec::new(),
        created_at: now_epoch(),
        updated_at: now_epoch(),
        tool_calls_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        daily_input_tokens: 0,
        daily_output_tokens: 0,
        input_token_source: crate::default_token_usage_source(),
        output_token_source: crate::default_token_usage_source(),
        token_usage_day: prompts::current_local_snapshot().today(),
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: crate::default_think_level(),
        show_react: crate::default_show_react(),
        show_tools: crate::default_show_tools(),
        show_reasoning: crate::default_show_reasoning(),
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: HashSet::new(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::empty(now_epoch()),
        pending_plan: None,
        version: crate::SESSION_VERSION,
        workspace,
    }
}

fn unique_temp_workspace(label: &str) -> std::path::PathBuf {
    let unique = NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "lingclaw-session-control-{label}-{}-{}-{unique}",
        std::process::id(),
        now_epoch()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("temp workspace should be created");
    path
}

#[test]
fn group_target_prompt_includes_context_and_instruction() {
    let prompt = target_prompt(
        Some("review-group"),
        "Check backend risk",
        Some("Recent messages:\n- main: split the review"),
    );

    assert!(prompt.contains("[Session group: review-group]"));
    assert!(prompt.contains("Group context summary:"));
    assert!(prompt.contains("Recent messages:"));
    assert!(prompt.contains("Main instruction:\nCheck backend risk"));
}

#[tokio::test]
async fn collect_and_target_group_context_redact_secrets() {
    let group_id = format!(
        "secret-group-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let mut group = SessionGroup::new(&group_id, "Secret Group", vec!["worker-a".to_string()]);
    group.runs.push(GroupRun {
        id: "run-secret".to_string(),
        group_id: group.id.clone(),
        session_id: "worker-a".to_string(),
        status: "completed".to_string(),
        prompt: "inspect".to_string(),
        result_excerpt: Some("API_KEY=secret-value".to_string()),
        error: Some("Authorization: Bearer super-secret".to_string()),
        created_at: 1,
        updated_at: 2,
        completed_at: Some(2),
    });
    group.messages.push(GroupMessage {
        id: "msg-secret".to_string(),
        role: "session".to_string(),
        session_id: Some("worker-a".to_string()),
        content: "password is hunter2".to_string(),
        timestamp: 3,
        turn_id: None,
        run_id: Some("run-secret".to_string()),
    });

    let summary = collect_group_summary(&group);
    assert!(summary.contains("[redacted]"));
    assert!(!summary.contains("secret-value"));
    assert!(!summary.contains("super-secret"));
    assert!(!summary.contains("hunter2"));

    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");
    let context = target_group_context(&group_id, 4_000);
    assert!(context.contains("[redacted]"));
    assert!(!context.contains("secret-value"));
    assert!(!context.contains("super-secret"));
    assert!(!context.contains("hunter2"));

    group_cleanup.cleanup_now();
}

#[tokio::test]
async fn record_group_session_result_redacts_persisted_message() {
    let state = test_app_state();
    let group_id = format!(
        "result-secret-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let group = SessionGroup::new(&group_id, "Result Secret", vec!["worker-a".to_string()]);
    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");

    record_group_session_result(
        &state,
        &group_id,
        "run-secret",
        "worker-a",
        "Authorization: Bearer super-secret".to_string(),
    )
    .await;

    let loaded = session_group::load_group_from_disk(&group_id).expect("group should load");
    let message = loaded.messages.last().expect("message should persist");
    assert!(message.content.contains("[redacted]"));
    assert!(!message.content.contains("super-secret"));
    group_cleanup.cleanup_now();
}

#[test]
fn stored_group_run_prompt_preserves_original_text() {
    let original = r#"Please review this literal text: password is rotated weekly"#;
    let prompt = stored_group_run_prompt(original);

    assert_eq!(prompt, original);
}

#[test]
fn direct_run_registry_tracks_terminal_status_for_waits() {
    let _guard = control_registry_test_guard();
    clear_direct_runs_for_test();
    let control = test_direct_control();
    register_direct_run("run-test-wait", "worker-a", &control);

    assert!(direct_run_is_active("run-test-wait"));

    update_direct_run_status("run-test-wait", DirectRunStatus::Running);
    assert!(direct_run_is_active("run-test-wait"));

    update_direct_run_status("run-test-wait", DirectRunStatus::Completed);
    assert!(direct_run_status("run-test-wait").is_terminal());
}

#[test]
fn group_run_status_transition_does_not_overwrite_terminal_state() {
    let mut completed = test_group_run("completed");
    completed.result_excerpt = Some("done".to_string());
    completed.completed_at = Some(2);

    assert!(apply_group_run_status_transition(&mut completed, "stopped", None, None, 3).is_none());
    assert_eq!(completed.status, "completed");
    assert_eq!(completed.result_excerpt.as_deref(), Some("done"));
    assert_eq!(completed.completed_at, Some(2));

    let mut stopped = test_group_run("queued");
    assert!(apply_group_run_status_transition(&mut stopped, "stopped", None, None, 4).is_some());
    assert_eq!(stopped.status, "stopped");
    assert!(apply_group_run_status_transition(&mut stopped, "running", None, None, 5).is_none());
    assert_eq!(stopped.status, "stopped");
}

#[tokio::test]
async fn update_run_status_noop_does_not_touch_group_updated_at() {
    let state = test_app_state();
    let group_id = format!(
        "noop-transition-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let mut group = SessionGroup::new(&group_id, "Noop Transition", vec!["worker-a".to_string()]);
    group.updated_at = 42;
    group.runs.push(GroupRun {
        id: "run-terminal".to_string(),
        group_id: group_id.clone(),
        session_id: "worker-a".to_string(),
        status: "completed".to_string(),
        prompt: "inspect".to_string(),
        result_excerpt: Some("done".to_string()),
        error: None,
        created_at: 1,
        updated_at: 2,
        completed_at: Some(2),
    });
    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");

    let updated = update_run_status(&state, &group_id, "run-terminal", "running", None, None)
        .await
        .expect("noop transition should not fail");

    assert!(updated.is_none());
    let loaded = session_group::load_group_from_disk(&group_id).expect("group should load");
    assert_eq!(loaded.updated_at, 42);
    assert_eq!(loaded.runs[0].status, "completed");
    group_cleanup.cleanup_now();
}

#[test]
fn group_session_status_merge_keeps_active_runs_over_terminal_runs() {
    let mut statuses = HashMap::new();

    merge_group_session_status(
        &mut statuses,
        "worker-a".to_string(),
        "queued".to_string(),
        1,
    );
    merge_group_session_status(
        &mut statuses,
        "worker-a".to_string(),
        "completed".to_string(),
        2,
    );

    assert_eq!(statuses["worker-a"].0, "queued");

    merge_group_session_status(
        &mut statuses,
        "worker-a".to_string(),
        "running".to_string(),
        1,
    );
    assert_eq!(statuses["worker-a"].0, "running");

    merge_group_session_status(
        &mut statuses,
        "worker-b".to_string(),
        "failed".to_string(),
        1,
    );
    merge_group_session_status(
        &mut statuses,
        "worker-b".to_string(),
        "completed".to_string(),
        2,
    );
    assert_eq!(statuses["worker-b"].0, "completed");
}

#[test]
fn direct_session_status_merge_keeps_active_runs_over_terminal_runs() {
    let mut statuses = HashMap::new();

    merge_direct_session_status(
        &mut statuses,
        "worker-a".to_string(),
        DirectRunStatus::Queued,
        1,
    );
    merge_direct_session_status(
        &mut statuses,
        "worker-a".to_string(),
        DirectRunStatus::Completed,
        2,
    );
    assert_eq!(statuses["worker-a"].0, DirectRunStatus::Queued);

    merge_direct_session_status(
        &mut statuses,
        "worker-a".to_string(),
        DirectRunStatus::Running,
        1,
    );
    assert_eq!(statuses["worker-a"].0, DirectRunStatus::Running);

    merge_direct_session_status(
        &mut statuses,
        "worker-b".to_string(),
        DirectRunStatus::Failed,
        1,
    );
    merge_direct_session_status(
        &mut statuses,
        "worker-b".to_string(),
        DirectRunStatus::Completed,
        2,
    );
    assert_eq!(statuses["worker-b"].0, DirectRunStatus::Completed);
}

#[test]
fn stop_direct_runs_cancels_queued_matching_targets() {
    let _guard = control_registry_test_guard();
    clear_direct_runs_for_test();
    let control = test_direct_control();
    register_direct_run("run-test-stop", "worker-stop-target", &control);

    let stopped = stop_direct_runs_for_targets(&["worker-stop-target".to_string()]);

    assert_eq!(stopped, 1);
    assert_eq!(direct_run_status("run-test-stop"), DirectRunStatus::Stopped);
    assert!(control.cancel.is_cancelled());
    assert!(control.stop_requested.load(Ordering::Relaxed));
    assert!(!direct_run_is_active("run-test-stop"));
}

#[test]
fn direct_run_terminal_status_is_not_overwritten() {
    let _guard = control_registry_test_guard();
    clear_direct_runs_for_test();
    let control = test_direct_control();
    register_direct_run("run-test-terminal", "worker-stop-target", &control);

    assert!(update_direct_run_status(
        "run-test-terminal",
        DirectRunStatus::Stopped
    ));
    assert!(!update_direct_run_status(
        "run-test-terminal",
        DirectRunStatus::Running
    ));
    assert!(!update_direct_run_status(
        "run-test-terminal",
        DirectRunStatus::Completed
    ));

    assert_eq!(
        direct_run_status("run-test-terminal"),
        DirectRunStatus::Stopped
    );
}

#[test]
fn zero_timeout_has_no_wait_deadline() {
    assert!(run_wait_deadline(Duration::ZERO).is_none());
    assert!(run_wait_deadline(Duration::from_millis(1)).is_some());
}

#[test]
fn validate_group_targets_rejects_non_members() {
    let members = vec!["worker-a".to_string(), "worker-b".to_string()];

    let error = validate_group_targets(
        "review-group",
        &members,
        &["worker-b".to_string(), "worker-c".to_string()],
    )
    .expect_err("non-member target should be rejected");

    assert!(error.contains("review-group"));
    assert!(error.contains("worker-c"));
}

#[test]
fn latest_assistant_content_after_ignores_previous_assistant() {
    let messages = vec![
        test_message("user", "old question"),
        test_message("assistant", "old answer"),
        test_message("user", "delegated task"),
    ];

    assert!(latest_assistant_content_after(&messages, 2, 1_000).is_none());

    let mut messages_with_reply = messages;
    messages_with_reply.push(test_message("assistant", "new answer"));

    assert_eq!(
        latest_assistant_content_after(&messages_with_reply, 2, 1_000).as_deref(),
        Some("new answer")
    );
}

#[test]
fn stop_group_run_controls_cancels_only_matching_group_runs() {
    let _guard = control_registry_test_guard();
    clear_group_run_controls_for_test();
    let matching = test_direct_control();
    let other_group = test_direct_control();
    register_group_run_control("run-group-stop", "group-a", "worker-a", &matching);
    register_group_run_control("run-other-group", "group-b", "worker-a", &other_group);

    let stopped = stop_group_run_controls("group-a", &["run-group-stop".to_string()]);

    assert_eq!(stopped, 1);
    assert!(matching.cancel.is_cancelled());
    assert!(matching.stop_requested.load(Ordering::Relaxed));
    assert!(!other_group.cancel.is_cancelled());
    assert!(!other_group.stop_requested.load(Ordering::Relaxed));
}

#[test]
fn profile_summary_prefers_frontmatter_and_marks_unfilled_templates() {
    let workspace = unique_temp_workspace("profile-summary");
    std::fs::write(
        workspace.join("AGENTS.md"),
        "---\nsummary: \"Focused backend reviewer\"\n---\n\n# Agent\nBody",
    )
    .unwrap();
    std::fs::write(
            workspace.join("IDENTITY.md"),
            "---\nsummary: \"Agent identity record\"\n---\n\n# IDENTITY.md - Agent Profile\n\n- **Name:**\n- **Role:**\n- **Style:**",
        )
        .unwrap();
    std::fs::write(
            workspace.join("USER.md"),
            "# USER.md - User Profile\n\n- **Name:**\n- **Preferred address:**\n- **Timezone:**\n\n## Preferences\n\n-",
        )
        .unwrap();

    let summary = summarize_session_profile(&workspace);

    assert_eq!(summary.agent.text, "Focused backend reviewer");
    assert_eq!(summary.identity.text, "Agent identity record");
    assert!(summary.identity.template_unfilled);
    assert!(summary.user.template_unfilled);

    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn profile_summary_redacts_secret_like_values() {
    let workspace = unique_temp_workspace("profile-redaction");
    std::fs::write(
        workspace.join("AGENTS.md"),
        "# Agent\n\nAuthorization: Bearer super-secret\nUse frontend review.\nPassword is hunter2.",
    )
    .unwrap();
    std::fs::write(
        workspace.join("IDENTITY.md"),
        "---\nsummary: \"Uses sk-live-1234567890abcdefghijklmnop for tests\"\n---\n\n# Identity",
    )
    .unwrap();
    std::fs::write(
            workspace.join("USER.md"),
            "# USER.md - User Profile\n\n- **Name:** Pat\n\n## Preferences\n\n- AWS_ACCESS_KEY_ID=AKIA1234567890ABCDEF keep concise\n- Legacy header Authorization: Basic basic-secret",
        )
        .unwrap();

    let summary = summarize_session_profile(&workspace);

    assert_eq!(
        summary.agent.text,
        "Authorization: [redacted] Use frontend review. Password: [redacted]"
    );
    assert_eq!(summary.identity.text, "Uses [redacted] for tests");
    assert_eq!(
        summary.user.text,
        "Name: Pat; Preferences: AWS_ACCESS_KEY_ID=[redacted] keep concise; Legacy header Authorization: [redacted]"
    );
    assert!(!summary.agent.text.contains("super-secret"));
    assert!(!summary.agent.text.contains("hunter2"));
    assert!(!summary.identity.text.contains("sk-live"));
    assert!(!summary.user.text.contains("AKIA"));
    assert!(!summary.user.text.contains("basic-secret"));

    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn profile_summary_redacts_natural_language_secret_labels() {
    assert_eq!(
        redact_profile_summary_text("password is hunter2"),
        "password: [redacted]"
    );
    assert_eq!(
        redact_profile_summary_text("api key is sk-live-1234567890abcdefghijklmnop"),
        "api key: [redacted]"
    );
    assert_eq!(
        redact_profile_summary_text("token: abc123"),
        "token: [redacted]"
    );
    let nested = redact_profile_summary_text(
        r#"{"config":{"api_key":"sk-live-1234567890abcdefghijklmnop"}}"#,
    );
    assert!(nested.contains("[redacted]"));
    assert!(!nested.contains("sk-live-1234567890abcdefghijklmnop"));
    let embedded = redact_profile_summary_text(r#"config {"authorization":"Bearer super-secret"}"#);
    assert!(embedded.contains("[redacted]"));
    assert!(!embedded.contains("super-secret"));
    let nested_json = redact_profile_summary_text(
        r#"{"notes":"password is hunter2","items":["token is abc123"]}"#,
    );
    assert!(nested_json.contains("[redacted]"));
    assert!(!nested_json.contains("hunter2"));
    assert!(!nested_json.contains("abc123"));
}

#[test]
fn strip_frontmatter_handles_crlf_and_bom() {
    let content =
        "\u{feff}---\r\nsummary: Agent summary\r\n---\r\n# Heading\r\n\n- **Role:** Reviewer\r\n";
    let stripped = strip_frontmatter(content);

    assert!(stripped.starts_with("# Heading"));
    assert!(stripped.contains("Role"));
}

#[test]
fn replace_frontmatter_summary_updates_bom_file_without_duplication() {
    let content = "\u{feff}---\r\nsummary: Old summary\r\n---\r\n\n# Agent\r\n";
    let updated = replace_or_insert_frontmatter_summary(content, "New summary");

    assert!(updated.starts_with('\u{feff}'));
    assert_eq!(updated.matches("---").count(), 2);
    assert_eq!(
        frontmatter_summary(&updated),
        Some("New summary".to_string())
    );
    assert!(updated.contains("# Agent"));
}

#[test]
fn placeholder_detection_keeps_real_todo_values() {
    assert!(!is_placeholder_value("TODO list approval first"));
    assert!(is_placeholder_value("TODO"));
    assert!(is_placeholder_value("TODO:"));
}

#[test]
fn unquoted_secret_redaction_preserves_json_array_tail() {
    let redacted = redact_profile_summary_text(r#"{"keys":["token=abc123","safe-text"]}"#);

    assert!(redacted.contains("token=[redacted]"));
    assert!(redacted.contains("safe-text"));
    assert!(!redacted.contains("abc123"));
}

#[test]
fn read_workspace_file_ignores_non_file_profile_paths() {
    let workspace = unique_temp_workspace("profile-non-file");
    std::fs::create_dir_all(workspace.join("AGENTS.md")).unwrap();

    assert!(read_workspace_file(&workspace, &["AGENTS.md"]).is_none());

    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn sanitize_profile_input_rejects_oversized_values() {
    let value = "a".repeat(CREATE_SESSION_PROFILE_MAX_CHARS + 1);
    let error = sanitize_profile_input("purpose", &value, CREATE_SESSION_PROFILE_MAX_CHARS)
        .expect_err("oversized profile should be rejected");

    assert!(error.contains("purpose exceeds"));
}

#[test]
fn sanitize_profile_input_redacts_json_and_multiline_secret_values() {
    let json_profile = sanitize_profile_input(
            "user_profile",
            r#"{"notes":"password is hunter2","mcp_headers":"x-custom-secret","items":["token is abc123"]}"#,
            CREATE_SESSION_PROFILE_MAX_CHARS,
        )
        .expect("json profile should sanitize");
    assert!(json_profile.contains("[redacted]"));
    assert!(!json_profile.contains("hunter2"));
    assert!(!json_profile.contains("x-custom-secret"));
    assert!(!json_profile.contains("abc123"));

    let multiline = sanitize_profile_input(
        "user_profile",
        "password:\nhunter2\nTimezone: UTC",
        CREATE_SESSION_PROFILE_MAX_CHARS,
    )
    .expect("multiline profile should sanitize");
    assert!(multiline.contains("password: [redacted]"));
    assert!(multiline.contains("[redacted]"));
    assert!(!multiline.contains("hunter2"));
    assert!(multiline.contains("Timezone: UTC"));

    let header_block = sanitize_profile_input(
            "user_profile",
            "headers: >\n  Authorization: Bearer super-secret\n  x-custom: still-secret\nRole: reviewer",
            CREATE_SESSION_PROFILE_MAX_CHARS,
        )
        .expect("header block should sanitize");
    assert!(header_block.contains("headers: [redacted]"));
    assert!(!header_block.contains("super-secret"));
    assert!(!header_block.contains("still-secret"));
    assert!(header_block.contains("Role: reviewer"));

    let header_map = sanitize_profile_input(
        "user_profile",
        "headers:\n  Authorization: Bearer super-secret\n  x-custom: still-secret\nRole: reviewer",
        CREATE_SESSION_PROFILE_MAX_CHARS,
    )
    .expect("header map should sanitize");
    assert!(header_map.contains("headers: [redacted]"));
    assert!(!header_map.contains("super-secret"));
    assert!(!header_map.contains("still-secret"));
    assert!(header_map.contains("Role: reviewer"));

    let private_key_block = sanitize_profile_input(
        "user_profile",
        "private_key: |\n  -----BEGIN PRIVATE KEY-----\n  very-secret-line\nTimezone: UTC",
        CREATE_SESSION_PROFILE_MAX_CHARS,
    )
    .expect("private key block should sanitize");
    assert!(private_key_block.contains("private_key: [redacted]"));
    assert!(!private_key_block.contains("BEGIN PRIVATE KEY"));
    assert!(!private_key_block.contains("very-secret-line"));
    assert!(private_key_block.contains("Timezone: UTC"));

    let private_key_lines = sanitize_profile_input(
        "user_profile",
        "private_key:\n  -----BEGIN PRIVATE KEY-----\n  very-secret-line\nTimezone: UTC",
        CREATE_SESSION_PROFILE_MAX_CHARS,
    )
    .expect("private key lines should sanitize");
    assert!(private_key_lines.contains("private_key: [redacted]"));
    assert!(!private_key_lines.contains("BEGIN PRIVATE KEY"));
    assert!(!private_key_lines.contains("very-secret-line"));
    assert!(private_key_lines.contains("Timezone: UTC"));
}

#[test]
fn describe_session_sections_are_deduplicated_in_order() {
    let sections = tool_args_sections(&json!({
        "sections": ["runtime", "runtime", "groups", "profile", "groups"]
    }));

    assert_eq!(sections, vec!["runtime", "groups", "profile"]);
}

#[test]
fn initialize_created_session_profile_writes_summaries_without_full_persona() {
    let workspace = unique_temp_workspace("profile-init");
    std::fs::write(
        workspace.join("AGENTS.md"),
        "---\nsummary: \"Main-agent workspace rules\"\n---\n\n# AGENTS.md - Main Agent Rules\n",
    )
    .unwrap();

    initialize_created_session_profile(
        &workspace,
        "Frontend Reviewer",
        Some("Review frontend UI and TypeScript changes"),
        Some("A precise frontend review agent."),
        Some("Works for the current LingClaw user."),
        Some("Concise and evidence-based."),
        Some("Use plan_only for risky changes."),
    )
    .unwrap();

    let profile = summarize_session_profile(&workspace);
    let agents = std::fs::read_to_string(workspace.join("AGENTS.md")).unwrap();

    assert_eq!(
        profile.agent.text,
        "Review frontend UI and TypeScript changes"
    );
    assert_eq!(profile.identity.text, "A precise frontend review agent.");
    assert_eq!(profile.user.text, "Works for the current LingClaw user.");
    assert!(agents.contains("## Session Control Profile"));
    assert!(agents.contains("### Agent Notes"));

    let _ = std::fs::remove_dir_all(workspace);
}

#[test]
fn initialize_created_session_profile_redacts_persisted_secrets() {
    let workspace = unique_temp_workspace("profile-init-redaction");
    std::fs::write(
        workspace.join("AGENTS.md"),
        "---\nsummary: \"Main-agent workspace rules\"\n---\n\n# AGENTS.md - Main Agent Rules\n",
    )
    .unwrap();

    let purpose = sanitize_profile_input(
        "purpose",
        "Review API_KEY=secret-value and Authorization: Bearer super-secret",
        CREATE_SESSION_PROFILE_MAX_CHARS,
    )
    .unwrap();
    let user_profile = sanitize_profile_input(
        "user_profile",
        "password is hunter2\nUse sk-live-1234567890abcdefghijklmnop only in tests",
        CREATE_SESSION_PROFILE_MAX_CHARS,
    )
    .unwrap();
    initialize_created_session_profile(
        &workspace,
        "Secret Safe Reviewer",
        Some(&purpose),
        None,
        Some(&user_profile),
        None,
        Some(
            &sanitize_profile_input(
                "agent_notes",
                "token: abc123",
                CREATE_SESSION_AGENT_NOTES_MAX_CHARS,
            )
            .unwrap(),
        ),
    )
    .unwrap();

    let agents = std::fs::read_to_string(workspace.join("AGENTS.md")).unwrap();
    let identity = std::fs::read_to_string(workspace.join("IDENTITY.md")).unwrap();
    let user = std::fs::read_to_string(workspace.join("USER.md")).unwrap();
    let combined = format!("{agents}\n{identity}\n{user}");

    assert!(combined.contains("[redacted]"));
    assert!(!combined.contains("secret-value"));
    assert!(!combined.contains("super-secret"));
    assert!(!combined.contains("hunter2"));
    assert!(!combined.contains("sk-live"));
    assert!(!combined.contains("abc123"));

    let _ = std::fs::remove_dir_all(workspace);
}

fn test_app_state() -> AppState {
    AppState {
        config: std::sync::Mutex::new(Arc::new(test_config())),
        http: reqwest::Client::new(),
        sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        active_connections: tokio::sync::Mutex::new(HashMap::new()),
        session_clients: tokio::sync::Mutex::new(HashMap::new()),
        group_clients: tokio::sync::Mutex::new(HashMap::new()),
        live_rounds: tokio::sync::Mutex::new(HashMap::new()),
        active_runs: tokio::sync::Mutex::new(HashMap::new()),
        connection_cancels: tokio::sync::Mutex::new(HashMap::new()),
        session_control_locks: tokio::sync::Mutex::new(HashMap::new()),
        next_connection_id: AtomicU64::new(1),
        shutdown: CancellationToken::new(),
        shutdown_token: "test-shutdown-token".to_string(),
        upload_token: "test-upload-token".to_string(),
        hooks: crate::HookRegistry::new(),
        memory_queue: std::sync::Mutex::new(None),
    }
}

fn test_config() -> crate::Config {
    crate::Config {
        api_key: "test-key".to_string(),
        api_base: "https://api.openai.com/v1".to_string(),
        model: "gpt-4o-mini".to_string(),
        fast_model: None,
        sub_agent_model: None,
        sub_agent_model_overrides: HashMap::new(),
        memory_model: None,
        reflection_model: None,
        context_model: None,
        provider: crate::Provider::OpenAI,
        openai_stream_include_usage: false,
        anthropic_prompt_caching: false,
        providers: HashMap::new(),
        mcp_servers: HashMap::new(),
        port: crate::DEFAULT_PORT,
        max_context_tokens: 32_000,
        exec_timeout: std::time::Duration::from_secs(30),
        tool_timeout: std::time::Duration::from_secs(30),
        sub_agent_timeout: std::time::Duration::from_secs(300),
        max_llm_retries: 2,
        max_output_bytes: 50 * 1024,
        max_file_bytes: 200 * 1024,
        structured_memory: false,
        daily_reflection: false,
        enable_state_digest: true,
        enable_task_plan: false,
        s3: None,
    }
}

#[tokio::test]
async fn execute_session_control_describe_session_covers_sections_and_errors() {
    let state = Arc::new(test_app_state());
    let workspace = unique_temp_workspace("describe-exec");
    let session =
        test_session_with_workspace("describe-test-session", "Describe Test", workspace.clone());
    std::fs::write(
        workspace.join("AGENTS.md"),
        "---\nsummary: \"Describe summary\"\n---\n\n# Agent\n",
    )
    .unwrap();
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session.id.clone(), session);
    }

    let outcome = execute_session_control_tool(
            &state,
            MAIN_SESSION_ID,
            r#"{"action":"describe_session","target":"describe-test-session","sections":["profile"],"max_chars":2000}"#,
        )
        .await;

    assert!(!outcome.is_error, "{}", outcome.output);
    assert!(outcome.output.contains("Session describe-test-session"));
    assert!(outcome.output.contains("Profile:"));
    assert!(outcome.output.contains("Describe summary"));
    assert!(!outcome.output.contains("Capabilities:"));

    let alias = execute_session_control_tool(
            &state,
            MAIN_SESSION_ID,
            r#"{"action":"describe_session","session_id":"describe-test-session","sections":["profile"],"max_chars":2000}"#,
        )
        .await;
    assert!(!alias.is_error, "{}", alias.output);
    assert!(alias.output.contains("Session describe-test-session"));

    let group_id = format!(
        "describe-secret-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let mut group = SessionGroup::new(
        &group_id,
        "Describe Secret Group",
        vec!["describe-test-session".to_string()],
    );
    group.runs.push(GroupRun {
        id: "run-secret".to_string(),
        group_id: group.id.clone(),
        session_id: "describe-test-session".to_string(),
        status: "completed".to_string(),
        prompt: "prompt".to_string(),
        result_excerpt: Some("API_KEY=secret-value".to_string()),
        error: Some("Authorization: Bearer super-secret".to_string()),
        created_at: 1,
        updated_at: 2,
        completed_at: Some(2),
    });
    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");

    let runtime = execute_session_control_tool(
            &state,
            MAIN_SESSION_ID,
            r#"{"action":"describe_session","target":"describe-test-session","sections":["runtime"],"max_chars":4000}"#,
        )
        .await;

    assert!(!runtime.is_error, "{}", runtime.output);
    assert!(runtime.output.contains("last_group_run"));
    assert!(runtime.output.contains("[redacted]"));
    assert!(!runtime.output.contains("secret-value"));
    assert!(!runtime.output.contains("super-secret"));

    let invalid = execute_session_control_tool(
        &state,
        MAIN_SESSION_ID,
        r#"{"action":"describe_session","target":"describe-test-session","sections":["bogus"]}"#,
    )
    .await;

    assert!(invalid.is_error);
    assert!(invalid.output.contains("invalid section"));

    let too_many_sections = execute_session_control_tool(
            &state,
            MAIN_SESSION_ID,
            r#"{"action":"describe_session","target":"describe-test-session","sections":["profile","capabilities","runtime","groups","profile"]}"#,
        )
        .await;

    assert!(too_many_sections.is_error);
    assert!(too_many_sections.output.contains("sections exceeds"));

    group_cleanup.cleanup_now();
    let _ = std::fs::remove_dir_all(workspace);
}

fn created_session_id_from_output(output: &str) -> Option<String> {
    output
        .strip_prefix("Created session ")?
        .split_whitespace()
        .next()
        .map(str::to_string)
}

struct CreatedGroupCleanup {
    group_id: Option<String>,
}

impl CreatedGroupCleanup {
    fn track(group_id: String) -> Self {
        Self {
            group_id: Some(group_id),
        }
    }

    fn cleanup_now(&mut self) {
        if let Some(group_id) = self.group_id.take() {
            cleanup_created_group_for_test(&group_id);
        }
    }
}

impl Drop for CreatedGroupCleanup {
    fn drop(&mut self) {
        self.cleanup_now();
    }
}

fn cleanup_created_group_for_test(group_id: &str) {
    let _ = std::fs::remove_file(session_group::group_path(group_id));
    let _ = std::fs::remove_file(session_group::groups_dir().join(format!("{group_id}.json.tmp")));
}

struct CreatedSessionCleanup {
    session_id: Option<String>,
}

impl CreatedSessionCleanup {
    fn new() -> Self {
        Self { session_id: None }
    }

    fn track(&mut self, session_id: String) {
        self.session_id = Some(session_id);
    }

    fn cleanup_now(&mut self) {
        if let Some(session_id) = self.session_id.take() {
            cleanup_created_session_for_test(&session_id);
        }
    }
}

impl Drop for CreatedSessionCleanup {
    fn drop(&mut self) {
        self.cleanup_now();
    }
}

fn cleanup_created_session_for_test(session_id: &str) {
    let _ = std::fs::remove_file(session_store::sessions_dir().join(format!("{session_id}.json")));
    let _ =
        std::fs::remove_file(session_store::sessions_dir().join(format!("{session_id}.json.tmp")));
    let _ = std::fs::remove_dir_all(crate::session_workspace_path(session_id));
}

#[tokio::test]
async fn execute_session_control_create_session_persists_redacted_profile() {
    let _guard = control_registry_test_guard();
    let state = Arc::new(test_app_state());
    let mut cleanup = CreatedSessionCleanup::new();

    let outcome = execute_session_control_tool(
            &state,
            MAIN_SESSION_ID,
            r#"{"action":"create_session","name":"E2E Reviewer","purpose":"Review API_KEY=secret-value","identity_profile":"Authorization: Bearer super-secret","user_profile":"password is hunter2","style_profile":"Concise","agent_notes":"token: abc123"}"#,
        )
        .await;

    assert!(!outcome.is_error, "{}", outcome.output);
    assert!(outcome.output.contains("Created session"));
    assert!(outcome.output.contains("E2E Reviewer"));
    assert!(outcome.output.contains("task_plan_global="));
    assert!(!outcome.output.contains(" task_plan="));
    assert!(outcome.output.contains("[redacted]"));
    assert!(!outcome.output.contains("secret-value"));
    assert!(!outcome.output.contains("super-secret"));
    assert!(!outcome.output.contains("hunter2"));
    assert!(!outcome.output.contains("abc123"));

    let session_id = created_session_id_from_output(&outcome.output).expect("created session id");
    cleanup.track(session_id.clone());
    let workspace = crate::session_workspace_path(&session_id);
    let loaded = session_store::load_session_from_disk(&session_id)
        .expect("created session should be saved");
    assert_eq!(loaded.name, "E2E Reviewer");

    let agents = std::fs::read_to_string(workspace.join("AGENTS.md")).unwrap_or_default();
    let identity = std::fs::read_to_string(workspace.join("IDENTITY.md")).unwrap_or_default();
    let user = std::fs::read_to_string(workspace.join("USER.md")).unwrap_or_default();
    let combined = format!("{agents}\n{identity}\n{user}");
    assert!(combined.contains("[redacted]"));
    assert!(!combined.contains("secret-value"));
    assert!(!combined.contains("super-secret"));
    assert!(!combined.contains("hunter2"));
    assert!(!combined.contains("abc123"));

    {
        let mut sessions = state.sessions.lock().await;
        sessions.remove(&session_id);
    }
    cleanup.cleanup_now();
}

#[tokio::test]
async fn execute_session_control_create_session_does_not_modify_existing_profiles() {
    let _guard = control_registry_test_guard();
    let state = Arc::new(test_app_state());
    let existing_workspace = unique_temp_workspace("existing-profile");
    std::fs::write(existing_workspace.join("AGENTS.md"), "existing agents").unwrap();
    std::fs::write(existing_workspace.join("IDENTITY.md"), "existing identity").unwrap();
    let existing = test_session_with_workspace(
        "existing-profile-session",
        "Existing",
        existing_workspace.clone(),
    );
    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(existing.id.clone(), existing);
    }
    let mut cleanup = CreatedSessionCleanup::new();

    let outcome = execute_session_control_tool(
        &state,
        MAIN_SESSION_ID,
        r#"{"action":"create_session","name":"New Reviewer","purpose":"Review only"}"#,
    )
    .await;

    assert!(!outcome.is_error, "{}", outcome.output);
    if let Some(session_id) = created_session_id_from_output(&outcome.output) {
        cleanup.track(session_id);
    }
    assert_eq!(
        std::fs::read_to_string(existing_workspace.join("AGENTS.md")).unwrap(),
        "existing agents"
    );
    assert_eq!(
        std::fs::read_to_string(existing_workspace.join("IDENTITY.md")).unwrap(),
        "existing identity"
    );

    cleanup.cleanup_now();
    let _ = std::fs::remove_dir_all(existing_workspace);
}

#[tokio::test]
async fn session_list_output_uses_persisted_model_override_for_unloaded_sessions() {
    let state = Arc::new(test_app_state());
    let session_id = format!("model-{}", NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed));
    let mut cleanup = CreatedSessionCleanup::new();
    cleanup.track(session_id.clone());
    let mut session = Session::new_with_id(&session_id, "Model Override Session");
    session.model_override = Some("claude-sonnet-4-6".to_string());
    session_store::save_session_to_disk_locked(&session)
        .await
        .expect("session should save");
    drop(session);

    let output = session_list_output(&state).await;

    assert!(output.contains(&format!(
        "- {session_id} (Model Override Session) model=claude-sonnet-4-6"
    )));
    assert!(output.contains("TaskPlan:"));
    assert!(output.contains("(global setting)"));
    assert!(!output.contains("task_plan="));
    assert!(output.contains("skills=unknown"));
    assert!(output.contains("mcp_tools=unknown"));
    assert!(output.contains("agent: unknown (use describe_session)"));
    assert!(output.contains("user: unknown (use describe_session)"));
    cleanup.cleanup_now();
}

#[tokio::test]
async fn active_group_run_statuses_preserves_queued_runs() {
    let _guard = control_registry_test_guard();
    let group_id = format!(
        "queued-status-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let mut group = SessionGroup::new(&group_id, "Queued Status", vec!["worker-a".to_string()]);
    group.runs.push(GroupRun {
        id: "queued-run".to_string(),
        group_id: group_id.clone(),
        session_id: "worker-a".to_string(),
        status: "queued".to_string(),
        prompt: "review".to_string(),
        result_excerpt: None,
        error: None,
        created_at: 1,
        updated_at: 1,
        completed_at: None,
    });
    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");
    let control = test_direct_control();
    register_group_run_control("queued-run", &group_id, "worker-a", &control);

    let statuses = active_group_run_statuses_by_session();

    assert_eq!(statuses.get("worker-a").map(String::as_str), Some("queued"));
    clear_group_run_control("queued-run");
    group_cleanup.cleanup_now();
}

#[cfg(windows)]
#[tokio::test]
async fn stop_group_runs_matches_windows_case_variant_targets() {
    let _guard = control_registry_test_guard();
    clear_group_run_controls_for_test();
    let state = test_app_state();
    let group_id = format!(
        "case-stop-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let mut group = SessionGroup::new(&group_id, "Case Stop", vec!["Worker-A".to_string()]);
    group.runs.push(GroupRun {
        id: "case-run".to_string(),
        group_id: group_id.clone(),
        session_id: "worker-a".to_string(),
        status: "queued".to_string(),
        prompt: "review".to_string(),
        result_excerpt: None,
        error: None,
        created_at: 1,
        updated_at: 1,
        completed_at: None,
    });
    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");
    let control = test_direct_control();
    register_group_run_control("case-run", &group_id, "worker-a", &control);

    let output = stop_group_runs(&state, &group_id, vec!["Worker-A".to_string()])
        .await
        .expect("case variant target should stop canonical run");

    assert!(output.contains("1 group run"));
    assert!(control.cancel.is_cancelled());
    assert!(control.stop_requested.load(Ordering::Relaxed));
    let loaded = session_group::load_group_from_disk(&group_id).expect("group should load");
    assert_eq!(loaded.runs[0].status, "stopped");
    clear_group_run_control("case-run");
    group_cleanup.cleanup_now();
}

#[tokio::test]
async fn session_control_rejects_oversized_dispatch_inputs() {
    let state = Arc::new(test_app_state());
    let long_message = "x".repeat(SESSION_CONTROL_MESSAGE_MAX_CHARS + 1);
    let outcome = execute_session_control_tool(
        &state,
        MAIN_SESSION_ID,
        &json!({
            "action": "dispatch",
            "targets": ["worker-a"],
            "message": long_message
        })
        .to_string(),
    )
    .await;

    assert!(outcome.is_error);
    assert!(outcome.output.contains("message exceeds"));

    let targets = (0..=SESSION_CONTROL_TARGETS_MAX_ITEMS)
        .map(|index| format!("worker-{index}"))
        .collect::<Vec<_>>();
    let outcome = execute_session_control_tool(
        &state,
        MAIN_SESSION_ID,
        &json!({
            "action": "dispatch",
            "targets": targets,
            "message": "review this"
        })
        .to_string(),
    )
    .await;

    assert!(outcome.is_error);
    assert!(outcome.output.contains("targets exceeds"));
}

#[tokio::test]
async fn session_control_rejects_non_array_targets() {
    let state = Arc::new(test_app_state());
    let group_id = format!(
        "non-array-targets-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let group = SessionGroup::new(&group_id, "Non Array Targets", vec!["worker-a".to_string()]);
    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");

    let outcome = execute_session_control_tool(
        &state,
        MAIN_SESSION_ID,
        &json!({
            "action": "dispatch",
            "group_id": group_id,
            "targets": "worker-a",
            "message": "review this"
        })
        .to_string(),
    )
    .await;

    assert!(outcome.is_error);
    assert!(outcome.output.contains("targets must be an array"));
    let loaded = session_group::load_group_from_disk(&group_id).expect("group should load");
    assert!(loaded.messages.is_empty());
    assert!(loaded.runs.is_empty());

    let outcome = execute_session_control_tool(
        &state,
        MAIN_SESSION_ID,
        &json!({
            "action": "dispatch",
            "group_id": group_id,
            "targets": [123],
            "message": "review this"
        })
        .to_string(),
    )
    .await;

    assert!(outcome.is_error);
    assert!(outcome.output.contains("targets must contain only strings"));
    let loaded = session_group::load_group_from_disk(&group_id).expect("group should load");
    assert!(loaded.messages.is_empty());
    assert!(loaded.runs.is_empty());
    group_cleanup.cleanup_now();
}

#[tokio::test]
async fn reserved_direct_run_stop_releases_before_prompt_append() {
    let _guard = control_registry_test_guard();
    clear_direct_runs_for_test();
    let state = Arc::new(test_app_state());
    let control = test_direct_control();
    let run = StartedRun {
        run_id: "run-stop-after-reserve".to_string(),
        group_id: None,
        session_id: "worker-stop-after-reserve".to_string(),
        control: control.clone(),
    };
    register_direct_run(&run.run_id, &run.session_id, &control);
    assert!(update_direct_run_status(
        &run.run_id,
        DirectRunStatus::Running
    ));
    let reservation = runtime_loop::try_reserve_agent_run(
        &state,
        &run.session_id,
        7,
        &control.cancel,
        &control.stop_requested,
    )
    .await
    .expect("run should reserve active slot");

    control.stop_requested.store(true, Ordering::Relaxed);
    control.cancel.cancel();

    assert!(
        release_reserved_run_if_stopped(
            &state,
            &run,
            &reservation,
            &control.cancel,
            &control.stop_requested,
        )
        .await
    );
    assert_eq!(direct_run_status(&run.run_id), DirectRunStatus::Stopped);
    assert!(!state.active_runs.lock().await.contains_key(&run.session_id));
    clear_direct_runs_for_test();
}

#[tokio::test]
async fn session_control_rejects_oversized_group_members() {
    let state = Arc::new(test_app_state());
    let members = (0..=SESSION_CONTROL_MEMBERS_MAX_ITEMS)
        .map(|index| format!("worker-{index}"))
        .collect::<Vec<_>>();

    let outcome = execute_session_control_tool(
        &state,
        MAIN_SESSION_ID,
        &json!({
            "action": "create_group",
            "name": "Too Large",
            "members": members
        })
        .to_string(),
    )
    .await;

    assert!(outcome.is_error);
    assert!(outcome.output.contains("members exceeds"));
}

#[tokio::test]
async fn group_socket_rejects_oversized_inputs() {
    let state = Arc::new(test_app_state());
    let group_id = format!(
        "socket-limits-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let members = (0..=SESSION_CONTROL_MEMBERS_MAX_ITEMS)
        .map(|index| format!("worker-{index}"))
        .collect::<Vec<_>>();
    let group = SessionGroup::new(&group_id, "Socket Limits", members);
    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");

    let error = handle_group_socket_message(
        &state,
        &group_id,
        GroupSocketDispatch {
            text: "x".repeat(SESSION_CONTROL_MESSAGE_MAX_CHARS + 1),
            targets: Vec::new(),
            target_mode: "all".to_string(),
            start_runs: false,
            run_mode: "execute".to_string(),
        },
    )
    .await
    .expect_err("oversized group message should be rejected");
    assert!(error.contains("message exceeds"));

    handle_group_socket_message(
        &state,
        &group_id,
        GroupSocketDispatch {
            text: "review".to_string(),
            targets: Vec::new(),
            target_mode: "all".to_string(),
            start_runs: false,
            run_mode: "execute".to_string(),
        },
    )
    .await
    .expect("plain group messages should not enforce dispatch target limits");
    let loaded = session_group::load_group_from_disk(&group_id).expect("group should load");
    assert!(
        loaded
            .messages
            .iter()
            .any(|message| message.content == "review")
    );

    let error = handle_group_socket_message(
        &state,
        &group_id,
        GroupSocketDispatch {
            text: "dispatch selected".to_string(),
            targets: vec!["worker-0".to_string(); SESSION_CONTROL_TARGETS_MAX_ITEMS + 1],
            target_mode: "selected".to_string(),
            start_runs: true,
            run_mode: "execute".to_string(),
        },
    )
    .await
    .expect_err("oversized raw selected target set should be rejected");
    assert!(error.contains("targets exceeds"));

    let error = handle_group_socket_message(
        &state,
        &group_id,
        GroupSocketDispatch {
            text: "dispatch".to_string(),
            targets: Vec::new(),
            target_mode: "all".to_string(),
            start_runs: true,
            run_mode: "execute".to_string(),
        },
    )
    .await
    .expect_err("oversized dispatch target set should be rejected");
    assert!(error.contains("targets exceeds"));

    let error = handle_group_socket_stop(
        &state,
        &group_id,
        vec!["worker-0".to_string(); SESSION_CONTROL_TARGETS_MAX_ITEMS + 1],
    )
    .await
    .expect_err("oversized stop target set should be rejected");
    assert!(error.contains("targets exceeds"));
    group_cleanup.cleanup_now();
}

#[tokio::test]
async fn group_broadcast_allows_member_cap_above_target_cap() {
    let state = Arc::new(test_app_state());
    let group_id = format!(
        "wide-broadcast-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let members = (0..=SESSION_CONTROL_TARGETS_MAX_ITEMS)
        .map(|index| format!("wide-worker-{index}"))
        .collect::<Vec<_>>();
    let mut workspaces = Vec::new();
    for member in &members {
        let workspace = unique_temp_workspace(member);
        workspaces.push(workspace.clone());
        let session = test_session_with_workspace(member, member, workspace);
        session_store::save_session_to_disk_locked(&session)
            .await
            .expect("session should save");
    }
    let group = SessionGroup::new(&group_id, "Wide Broadcast", members.clone());
    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");

    let targets = prepare_dispatch_targets(
        &state,
        Some(&group_id),
        Vec::new(),
        SESSION_CONTROL_MEMBERS_MAX_ITEMS,
    )
    .await
    .expect("group fallback targets should allow all members");

    assert_eq!(targets.len(), SESSION_CONTROL_TARGETS_MAX_ITEMS + 1);
    for member in members {
        cleanup_created_session_for_test(&member);
    }
    for workspace in workspaces {
        let _ = std::fs::remove_dir_all(workspace);
    }
    group_cleanup.cleanup_now();
}

#[tokio::test]
async fn group_socket_mentions_without_valid_target_is_rejected() {
    let state = Arc::new(test_app_state());
    let group_id = format!(
        "mentions-empty-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let group = SessionGroup::new(
        &group_id,
        "Mentions Empty",
        vec!["worker-a".to_string(), "worker-b".to_string()],
    );
    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");

    let error = handle_group_socket_message(
        &state,
        &group_id,
        GroupSocketDispatch {
            text: "worker-a please review".to_string(),
            targets: Vec::new(),
            target_mode: "mentions".to_string(),
            start_runs: true,
            run_mode: "execute".to_string(),
        },
    )
    .await
    .expect_err("mentions mode without @session-id should be rejected");

    assert!(error.contains("No valid @session-id mentions"));
    let loaded = session_group::load_group_from_disk(&group_id).expect("group should load");
    assert!(loaded.messages.is_empty());
    assert!(loaded.runs.is_empty());
    group_cleanup.cleanup_now();
}

#[tokio::test]
async fn session_control_dispatch_requires_existing_target_session() {
    let state = Arc::new(test_app_state());
    let missing_id = format!(
        "missing-dispatch-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    cleanup_created_session_for_test(&missing_id);

    let outcome = execute_session_control_tool(
        &state,
        MAIN_SESSION_ID,
        &json!({
            "action": "dispatch",
            "targets": [missing_id],
            "message": "review this"
        })
        .to_string(),
    )
    .await;

    assert!(outcome.is_error);
    assert!(outcome.output.contains("not found"));
    assert!(!crate::session_workspace_path(&missing_id).exists());
    cleanup_created_session_for_test(&missing_id);
}

#[tokio::test]
async fn dispatch_target_resolution_rejects_sessions_loaded_as_main() {
    let state = Arc::new(test_app_state());
    let spoof_id = format!(
        "spoof-main-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let workspace = unique_temp_workspace("spoof-main");
    let session = test_session_with_workspace(MAIN_SESSION_ID, "Spoof Main", workspace.clone());
    std::fs::create_dir_all(session_store::sessions_dir()).expect("sessions dir should exist");
    let path = session_store::sessions_dir().join(format!("{spoof_id}.json"));
    std::fs::write(
        &path,
        serde_json::to_string(&session).expect("session should serialize"),
    )
    .expect("spoof session should write");

    let result = resolve_existing_target_session_ids(&state, vec![spoof_id.clone()]).await;

    assert!(result.is_err());
    assert!(
        result
            .expect_err("main spoof should be rejected")
            .contains("cannot dispatch to the main session")
    );
    let _ = std::fs::remove_file(path);
    let _ =
        std::fs::remove_file(session_store::sessions_dir().join(format!("{spoof_id}.json.tmp")));
    let _ = std::fs::remove_dir_all(workspace);
}

#[tokio::test]
async fn group_dispatch_validation_failure_does_not_append_message() {
    let state = Arc::new(test_app_state());
    let group_id = format!(
        "dispatch-invalid-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let group = SessionGroup::new(&group_id, "Dispatch Invalid", vec!["worker-a".to_string()]);
    let original_updated_at = group.updated_at;
    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");

    let outcome = execute_session_control_tool(
        &state,
        MAIN_SESSION_ID,
        &json!({
            "action": "dispatch",
            "group_id": group_id,
            "targets": ["not-a-member"],
            "message": "should not persist"
        })
        .to_string(),
    )
    .await;

    assert!(outcome.is_error);
    let loaded = session_group::load_group_from_disk(&group_id).expect("group should load");
    assert!(loaded.messages.is_empty());
    assert_eq!(loaded.updated_at, original_updated_at);
    group_cleanup.cleanup_now();
}

#[tokio::test]
async fn group_dispatch_revalidates_current_members_before_writing() {
    let state = Arc::new(test_app_state());
    let group_id = format!(
        "dispatch-revalidate-{}",
        NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed)
    );
    let mut group_cleanup = CreatedGroupCleanup::track(group_id.clone());
    let group = SessionGroup::new(&group_id, "Dispatch Revalidate", Vec::new());
    session_group::save_group_to_disk_locked(&group)
        .await
        .expect("group should save");

    let result = dispatch_to_sessions(
        &state,
        DispatchRequest {
            group_id: Some(group_id.clone()),
            targets: vec!["worker-a".to_string()],
            message: "should not persist".to_string(),
            group_message: Some(DispatchGroupMessage {
                role: "main".to_string(),
                session_id: None,
                turn_id: None,
            }),
            run_mode: AgentRunMode::Execute,
            wait: false,
            summary_budget: 4_000,
        },
    )
    .await;

    assert!(result.is_err());
    let loaded = session_group::load_group_from_disk(&group_id).expect("group should load");
    assert!(loaded.messages.is_empty());
    assert!(loaded.runs.is_empty());
    group_cleanup.cleanup_now();
}

#[test]
fn session_control_schema_includes_discovery_and_create_actions() {
    let schema = tools::session_control_tool_parameters();
    let actions = schema["properties"]["action"]["enum"]
        .as_array()
        .expect("action enum should be an array")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();

    assert!(actions.contains(&"list_sessions"));
    assert!(actions.contains(&"create_session"));
    assert!(actions.contains(&"describe_session"));
    assert!(
        schema["properties"]
            .as_object()
            .unwrap()
            .contains_key("sections")
    );
    assert!(
        schema["properties"]
            .as_object()
            .unwrap()
            .contains_key("session_id")
    );
    assert!(
        schema["properties"]
            .as_object()
            .unwrap()
            .contains_key("max_chars")
    );
    assert_eq!(
        schema["properties"]["message"]["maxLength"].as_u64(),
        Some(32_000)
    );
    assert_eq!(
        schema["properties"]["targets"]["maxItems"].as_u64(),
        Some(16)
    );
    assert_eq!(
        schema["properties"]["members"]["maxItems"].as_u64(),
        Some(64)
    );
}
