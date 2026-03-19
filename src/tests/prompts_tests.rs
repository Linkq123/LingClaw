use super::*;
use std::fs;

#[test]
fn test_local_datetime_formatters() {
    let date_time =
        DateTime::parse_from_rfc3339("2026-03-16T00:05:07+08:00").expect("datetime should parse");

    assert_eq!(format_local_date(date_time), "2026-03-16");
    assert_eq!(format_local_hhmm(date_time), "00:05");
    assert_eq!(
        format_local_datetime_label(date_time),
        "2026-03-16 00:05:07 +08:00"
    );
}

#[test]
fn local_time_snapshot_uses_single_now_across_midnight_boundaries() {
    let snapshot = LocalTimeSnapshot::from_datetime(
        DateTime::parse_from_rfc3339("2026-03-16T00:05:07+08:00").expect("datetime should parse"),
    );

    assert_eq!(snapshot.today(), "2026-03-16");
    assert_eq!(snapshot.yesterday(), "2026-03-15");
    assert_eq!(snapshot.hhmm(), "00:05");
    assert_eq!(snapshot.datetime_label(), "2026-03-16 00:05:07 +08:00");
}

#[test]
fn load_session_prompt_files_uses_same_snapshot_for_today_and_yesterday() {
    let workspace = std::env::temp_dir().join("lingclaw-prompt-snapshot-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(workspace.join("memory")).expect("memory dir should be created");
    fs::write(workspace.join("AGENTS.md"), "agent").expect("agent file should be written");
    fs::write(workspace.join("IDENTITY.md"), "identity").expect("identity file should be written");
    fs::write(workspace.join("USER.md"), "user").expect("user file should be written");
    fs::write(workspace.join("SOUL.md"), "soul").expect("soul file should be written");
    fs::write(workspace.join("memory/2026-03-16.md"), "today memory")
        .expect("today memory should be written");
    fs::write(workspace.join("memory/2026-03-15.md"), "yesterday memory")
        .expect("yesterday memory should be written");

    let snapshot = LocalTimeSnapshot::from_datetime(
        DateTime::parse_from_rfc3339("2026-03-16T00:05:07+08:00").expect("datetime should parse"),
    );
    let loaded = load_session_prompt_files_with_snapshot(&workspace, snapshot);

    assert!(loaded.contains("<!-- memory/2026-03-16.md -->\ntoday memory"));
    assert!(loaded.contains("<!-- memory/2026-03-15.md -->\nyesterday memory"));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn load_session_prompt_files_auto_completes_bootstrap_when_identity_is_edited() {
    let workspace = std::env::temp_dir().join("lingclaw-bootstrap-identity-edit-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    fs::create_dir_all(workspace.join("memory")).expect("memory dir should be created");
    fs::write(workspace.join("BOOTSTRAP.md"), "bootstrap")
        .expect("bootstrap file should be written");
    fs::write(workspace.join("AGENTS.md"), "agent").expect("agent file should be written");
    fs::write(
        workspace.join("IDENTITY.md"),
        "- Name: Ling\n- Creature:\n- Vibe:\n- Emoji:\n- Avatar: none\n",
    )
    .expect("identity file should be written");
    fs::write(
        workspace.join("USER.md"),
        template_file_content("USER.md").expect("user template should exist"),
    )
    .expect("user file should be written");
    fs::write(workspace.join("SOUL.md"), "soul").expect("soul file should be written");

    let snapshot = LocalTimeSnapshot::from_datetime(
        DateTime::parse_from_rfc3339("2026-03-16T00:05:07+08:00").expect("datetime should parse"),
    );
    let loaded = load_session_prompt_files_with_snapshot(&workspace, snapshot);

    assert!(!workspace.join("BOOTSTRAP.md").exists());
    assert!(!loaded.contains("<!-- BOOTSTRAP.md -->"));
    assert!(loaded.contains("<!-- AGENTS.md -->\nagent"));
    assert!(loaded.contains("<!-- IDENTITY.md -->"));
    assert!(loaded.contains("<!-- USER.md -->"));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn load_session_prompt_files_keeps_bootstrap_until_profile_files_change() {
    let workspace = std::env::temp_dir().join("lingclaw-bootstrap-incomplete-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    fs::write(workspace.join("BOOTSTRAP.md"), "bootstrap")
        .expect("bootstrap file should be written");
    fs::write(workspace.join("AGENTS.md"), "agent").expect("agent file should be written");
    fs::write(
        workspace.join("IDENTITY.md"),
        template_file_content("IDENTITY.md").expect("identity template should exist"),
    )
    .expect("identity file should be written");
    fs::write(
        workspace.join("USER.md"),
        template_file_content("USER.md").expect("user template should exist"),
    )
    .expect("user file should be written");

    let snapshot = LocalTimeSnapshot::from_datetime(
        DateTime::parse_from_rfc3339("2026-03-16T00:05:07+08:00").expect("datetime should parse"),
    );
    let loaded = load_session_prompt_files_with_snapshot(&workspace, snapshot);

    assert!(workspace.join("BOOTSTRAP.md").exists());
    assert!(loaded.contains("<!-- BOOTSTRAP.md -->\nbootstrap"));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn load_session_prompt_files_auto_completes_bootstrap_when_user_is_edited() {
    let workspace = std::env::temp_dir().join("lingclaw-bootstrap-user-edit-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    fs::write(workspace.join("BOOTSTRAP.md"), "bootstrap")
        .expect("bootstrap file should be written");
    fs::write(workspace.join("AGENTS.md"), "agent").expect("agent file should be written");
    fs::write(
        workspace.join("IDENTITY.md"),
        template_file_content("IDENTITY.md").expect("identity template should exist"),
    )
    .expect("identity file should be written");
    fs::write(
        workspace.join("USER.md"),
        "- **Name:** Alex\n- **What to call them:**\n- **Timezone:**\n",
    )
    .expect("user file should be written");

    let snapshot = LocalTimeSnapshot::from_datetime(
        DateTime::parse_from_rfc3339("2026-03-16T00:05:07+08:00").expect("datetime should parse"),
    );
    let loaded = load_session_prompt_files_with_snapshot(&workspace, snapshot);

    assert!(!workspace.join("BOOTSTRAP.md").exists());
    assert!(!loaded.contains("<!-- BOOTSTRAP.md -->"));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn load_session_prompt_files_auto_completes_bootstrap_when_values_are_appended_below_placeholders()
{
    let workspace = std::env::temp_dir().join("lingclaw-bootstrap-appended-values-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    fs::write(workspace.join("BOOTSTRAP.md"), "bootstrap")
        .expect("bootstrap file should be written");
    fs::write(workspace.join("AGENTS.md"), "agent").expect("agent file should be written");
    fs::write(
        workspace.join("IDENTITY.md"),
        "- Name:\n- Creature:\n- Vibe:\n- Emoji:\n- Name: Ling\n- Creature: assistant\n- Vibe: calm\n- Emoji: ✨\n",
    )
    .expect("identity file should be written");
    fs::write(
        workspace.join("USER.md"),
        "- **Name:**\n- **What to call them:**\n- **Timezone:**\n- **Name:** Alex\n- **What to call them:** Alex\n- **Timezone:** Asia/Shanghai\n",
    )
    .expect("user file should be written");

    let snapshot = LocalTimeSnapshot::from_datetime(
        DateTime::parse_from_rfc3339("2026-03-16T00:05:07+08:00").expect("datetime should parse"),
    );
    let loaded = load_session_prompt_files_with_snapshot(&workspace, snapshot);

    assert!(!workspace.join("BOOTSTRAP.md").exists());
    assert!(!loaded.contains("<!-- BOOTSTRAP.md -->"));
    assert!(loaded.contains("<!-- IDENTITY.md -->"));
    assert!(loaded.contains("<!-- USER.md -->"));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn bootstrap_completion_uses_session_baseline_instead_of_current_template() {
    let workspace = std::env::temp_dir().join("lingclaw-bootstrap-baseline-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    fs::create_dir_all(workspace.join(BOOTSTRAP_BASELINE_DIR))
        .expect("baseline dir should be created");
    fs::write(workspace.join("BOOTSTRAP.md"), "bootstrap")
        .expect("bootstrap file should be written");
    fs::write(workspace.join("AGENTS.md"), "agent").expect("agent file should be written");

    let baseline_identity = "old identity template\n";
    let baseline_user = "old user template\n";
    fs::write(workspace.join("IDENTITY.md"), baseline_identity)
        .expect("identity file should be written");
    fs::write(workspace.join("USER.md"), baseline_user).expect("user file should be written");
    fs::write(
        bootstrap_baseline_path(&workspace, "IDENTITY.md"),
        baseline_identity,
    )
    .expect("identity baseline should be written");
    fs::write(
        bootstrap_baseline_path(&workspace, "USER.md"),
        baseline_user,
    )
    .expect("user baseline should be written");

    let snapshot = LocalTimeSnapshot::from_datetime(
        DateTime::parse_from_rfc3339("2026-03-16T00:05:07+08:00").expect("datetime should parse"),
    );
    let loaded = load_session_prompt_files_with_snapshot(&workspace, snapshot);

    assert!(workspace.join("BOOTSTRAP.md").exists());
    assert!(loaded.contains("<!-- BOOTSTRAP.md -->\nbootstrap"));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn ensure_session_workspace_migrates_legacy_agent_file() {
    let workspace = std::env::temp_dir().join("lingclaw-agent-rename-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    fs::write(workspace.join("AGENT.md"), "legacy agent").expect("legacy agent should be written");

    ensure_session_workspace(&workspace);

    assert!(!workspace.join("AGENT.md").exists());
    assert_eq!(
        fs::read_to_string(workspace.join("AGENTS.md")).expect("renamed agent should be readable"),
        "legacy agent"
    );

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn parse_identity_avatar_supports_english_avatar_key() {
    let workspace = std::env::temp_dir().join("lingclaw-avatar-english-key-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    fs::write(workspace.join("IDENTITY.md"), "- Avatar: ✨\n")
        .expect("identity file should be written");

    let avatar = parse_identity_avatar(&workspace);

    assert_eq!(avatar.as_deref(), Some("✨"));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn parse_identity_avatar_supports_bold_avatar_key() {
    let workspace = std::env::temp_dir().join("lingclaw-avatar-bold-key-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    fs::write(workspace.join("IDENTITY.md"), "- **Avatar:** avatar.png\n")
        .expect("identity file should be written");
    fs::write(workspace.join("avatar.png"), "not a real png but present")
        .expect("avatar file should be written");

    let avatar = parse_identity_avatar(&workspace);

    assert!(avatar.is_some());

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn parse_identity_avatar_treats_inline_none_guidance_as_unset() {
    let workspace = std::env::temp_dir().join("lingclaw-avatar-inline-none-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    fs::write(
        workspace.join("IDENTITY.md"),
        "- 头像：none （未设置时填写 none；也可填写工作区相对路径、http(s) URL、data URI）\n",
    )
    .expect("identity file should be written");

    let avatar = parse_identity_avatar(&workspace);

    assert_eq!(avatar, None);

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn parse_identity_avatar_keeps_text_that_only_starts_with_none() {
    let workspace = std::env::temp_dir().join("lingclaw-avatar-none-prefix-text-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    fs::write(workspace.join("IDENTITY.md"), "- 头像：none-core\n")
        .expect("identity file should be written");

    let avatar = parse_identity_avatar(&workspace);

    assert_eq!(avatar.as_deref(), Some("none-core"));

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn parse_identity_avatar_treats_case_mixed_inline_none_guidance_as_unset() {
    let workspace = std::env::temp_dir().join("lingclaw-avatar-none-mixed-case-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    fs::write(
        workspace.join("IDENTITY.md"),
        "- 头像：None (leave unset)\n",
    )
    .expect("identity file should be written");

    let avatar = parse_identity_avatar(&workspace);

    assert_eq!(avatar, None);

    let _ = fs::remove_dir_all(&workspace);
}
