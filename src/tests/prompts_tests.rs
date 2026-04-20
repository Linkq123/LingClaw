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
        "- Name: Ling\n- Creature:\n- Vibe:\n- Emoji:\n",
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

// ── Skill discovery tests ────────────────────────────────────────────────────────────────

#[test]
fn parse_skill_frontmatter_extracts_name_and_description() {
    let content = "---\nname: my-skill\ndescription: Does something useful\n---\n\n# Instructions";
    let meta = parse_skill_frontmatter(content).expect("frontmatter should parse");
    assert_eq!(meta.name, "my-skill");
    assert_eq!(meta.description, "Does something useful");
}

#[test]
fn parse_skill_frontmatter_handles_quoted_values() {
    let content = "---\nname: \"quoted-skill\"\ndescription: 'single quoted'\n---\n";
    let meta = parse_skill_frontmatter(content).expect("frontmatter should parse");
    assert_eq!(meta.name, "quoted-skill");
    assert_eq!(meta.description, "single quoted");
}

#[test]
fn parse_skill_frontmatter_returns_none_without_name() {
    let content = "---\ndescription: orphan description\n---\n";
    assert!(parse_skill_frontmatter(content).is_none());
}

#[test]
fn parse_skill_frontmatter_returns_none_without_frontmatter() {
    let content = "# No frontmatter\nJust instructions";
    assert!(parse_skill_frontmatter(content).is_none());
}

#[test]
fn parse_skill_frontmatter_allows_empty_description() {
    let content = "---\nname: minimal\n---\n";
    let meta = parse_skill_frontmatter(content).expect("frontmatter should parse");
    assert_eq!(meta.name, "minimal");
    assert!(meta.description.is_empty());
}

#[test]
fn parse_skill_frontmatter_ignores_extra_fields() {
    let content =
        "---\nname: extended\ndescription: A skill\nlicense: MIT\nversion: 1.0\n---\n# Body";
    let meta = parse_skill_frontmatter(content).expect("frontmatter should parse");
    assert_eq!(meta.name, "extended");
    assert_eq!(meta.description, "A skill");
}

#[test]
fn discover_skills_finds_valid_skills_in_workspace() {
    let workspace = std::env::temp_dir().join("lingclaw-skill-discovery-test");
    let _ = fs::remove_dir_all(&workspace);
    let skills_dir = workspace.join("skills");

    // Create two valid skills
    let skill_a = skills_dir.join("alpha");
    fs::create_dir_all(&skill_a).expect("skill dir should be created");
    fs::write(
        skill_a.join("SKILL.md"),
        "---\nname: alpha\ndescription: First skill\n---\n# Alpha",
    )
    .expect("skill file should be written");

    let skill_b = skills_dir.join("beta");
    fs::create_dir_all(&skill_b).expect("skill dir should be created");
    fs::write(
        skill_b.join("SKILL.md"),
        "---\nname: beta\ndescription: Second skill\n---\n# Beta",
    )
    .expect("skill file should be written");

    // Create an invalid entry (file, not dir)
    fs::write(skills_dir.join("not-a-skill.txt"), "junk").expect("junk file should be written");

    // Create a dir without SKILL.md
    let no_skill = skills_dir.join("empty");
    fs::create_dir_all(&no_skill).expect("empty dir should be created");

    let skills = discover_skills_by_source(&workspace, SkillSource::Session);
    assert_eq!(skills.len(), 2);
    assert_eq!(skills[0].name, "alpha");
    assert_eq!(skills[0].path, "skills/alpha/SKILL.md");
    assert_eq!(skills[0].source, SkillSource::Session);
    assert_eq!(skills[1].name, "beta");
    assert_eq!(skills[1].path, "skills/beta/SKILL.md");
    assert_eq!(skills[1].source, SkillSource::Session);

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn discover_skills_returns_empty_when_no_skills_dir() {
    let workspace = std::env::temp_dir().join("lingclaw-no-skills-dir-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");
    assert!(discover_skills_by_source(&workspace, SkillSource::Session).is_empty());
    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn render_skills_catalog_formats_correctly() {
    let skills = vec![
        SkillMeta {
            name: "coding".to_string(),
            description: "Help with code".to_string(),
            path: "skills/coding/SKILL.md".to_string(),
            source: SkillSource::Session,
        },
        SkillMeta {
            name: "minimal".to_string(),
            description: String::new(),
            path: "skills/minimal/SKILL.md".to_string(),
            source: SkillSource::System,
        },
    ];
    let catalog = render_skills_catalog(&skills, None).expect("catalog should render");
    assert!(catalog.contains("## Skills"));
    assert!(catalog.contains("**coding** [`session`] — Help with code (`skills/coding/SKILL.md`)"));
    assert!(catalog.contains("**minimal** [`system`] (`skills/minimal/SKILL.md`)"));
}

#[test]
fn render_skills_catalog_returns_none_for_empty_list() {
    assert!(render_skills_catalog(&[], None).is_none());
}

#[test]
fn init_session_creates_skills_directory() {
    let workspace = std::env::temp_dir().join("lingclaw-init-skills-dir-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");

    init_session_prompt_files(&workspace);

    assert!(workspace.join("skills").is_dir());
    assert!(workspace.join("memory").is_dir());

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn discover_all_skills_merges_session_skills() {
    let workspace = std::env::temp_dir().join("lingclaw-all-skills-merge-test");
    let _ = fs::remove_dir_all(&workspace);
    let skills_dir = workspace.join("skills");

    let skill_a = skills_dir.join("alpha");
    fs::create_dir_all(&skill_a).expect("skill dir should be created");
    fs::write(
        skill_a.join("SKILL.md"),
        "---\nname: alpha\ndescription: Session alpha\n---\n",
    )
    .expect("skill file should be written");

    let skills = discover_all_skills(&workspace);
    // At minimum, the session skill should be present
    assert!(
        skills
            .iter()
            .any(|s| s.name == "alpha" && s.source == SkillSource::Session)
    );

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn discover_all_skills_deduplicates_by_name_later_wins() {
    // End-to-end dedup: two separate dirs with an overlapping skill name.
    // Mirrors the merge logic inside discover_all_skills().
    let base = std::env::temp_dir().join("lingclaw-dedup-e2e-test");
    let _ = fs::remove_dir_all(&base);

    let system_dir = base.join("system");
    let session_dir = base.join("session");

    // "xray" exists in both layers — session version should win
    let sys_xray = system_dir.join("xray");
    fs::create_dir_all(&sys_xray).expect("sys skill dir should be created");
    fs::write(
        sys_xray.join("SKILL.md"),
        "---\nname: xray\ndescription: System version\n---\n",
    )
    .expect("sys skill file should be written");

    let ses_xray = session_dir.join("xray");
    fs::create_dir_all(&ses_xray).expect("ses skill dir should be created");
    fs::write(
        ses_xray.join("SKILL.md"),
        "---\nname: xray\ndescription: Session version\n---\n",
    )
    .expect("ses skill file should be written");

    // "only-sys" only in system — should survive dedup
    let sys_unique = system_dir.join("only-sys");
    fs::create_dir_all(&sys_unique).expect("unique skill dir should be created");
    fs::write(
        sys_unique.join("SKILL.md"),
        "---\nname: only-sys\ndescription: Only in system\n---\n",
    )
    .expect("unique skill file should be written");

    // Simulate the exact merge+dedup from discover_all_skills
    let mut all = Vec::new();
    all.extend(discover_skills_in_dir(
        &system_dir,
        SkillSource::System,
        "system://skills/",
    ));
    all.extend(discover_skills_in_dir(
        &session_dir,
        SkillSource::Session,
        "skills/",
    ));

    let mut seen = std::collections::HashMap::new();
    for (idx, skill) in all.iter().enumerate() {
        seen.insert(skill.name.clone(), idx);
    }
    let mut deduped: Vec<SkillMeta> = seen.into_values().map(|idx| all[idx].clone()).collect();
    deduped.sort_by(|a, b| a.name.cmp(&b.name));

    // Two unique names after dedup
    assert_eq!(deduped.len(), 2);

    // "only-sys" retained from system layer
    let unique = deduped.iter().find(|s| s.name == "only-sys").unwrap();
    assert_eq!(unique.source, SkillSource::System);

    // "xray" resolved to session (later layer wins)
    let xray = deduped.iter().find(|s| s.name == "xray").unwrap();
    assert_eq!(xray.source, SkillSource::Session);
    assert_eq!(xray.description, "Session version");

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn test_skill_tokenize_handles_cjk() {
    let tokens = crate::tokenize_for_matching("PDF解析工具");
    assert_eq!(tokens, vec!["pdf", "解", "析", "工", "具"]);

    let tokens = crate::tokenize_for_matching("hello world");
    assert_eq!(tokens, vec!["hello", "world"]);

    let tokens = crate::tokenize_for_matching("代码审查");
    assert_eq!(tokens, vec!["代", "码", "审", "查"]);
}

#[test]
fn test_skill_tokenize_excludes_cjk_punctuation() {
    // CJK punctuation (。？「」) should NOT become tokens
    let tokens = crate::tokenize_for_matching("你好。世界？");
    assert_eq!(tokens, vec!["你", "好", "世", "界"]);
}

#[test]
fn test_render_skills_catalog_query_aware_compression() {
    // Build 6 skills (above SKILL_FULL_DISPLAY_THRESHOLD=5) with one matching query
    let skills: Vec<SkillMeta> = (0..6)
        .map(|i| SkillMeta {
            name: format!("skill-{i}"),
            description: if i == 2 {
                "Help with Rust code".to_string()
            } else {
                format!("Generic skill {i}")
            },
            path: format!("skills/skill-{i}/SKILL.md"),
            source: SkillSource::Session,
        })
        .collect();

    // With a relevant query, top-N get full descriptions, rest abbreviated
    let catalog = render_skills_catalog(&skills, Some("Rust")).unwrap();
    // skill-2 has "Rust" overlap, should get full description
    assert!(catalog.contains("Help with Rust code"));
    // At least one skill should be abbreviated (no description shown)
    let abbreviated_count = catalog
        .lines()
        .filter(|l| l.starts_with("- **skill-") && !l.contains(" — "))
        .count();
    assert!(abbreviated_count > 0, "some skills should be abbreviated");
}

#[test]
fn test_render_skills_catalog_zero_hit_shows_all_descriptions() {
    // 6 skills, none matching the query — should fall back to full display
    let skills: Vec<SkillMeta> = (0..6)
        .map(|i| SkillMeta {
            name: format!("skill-{i}"),
            description: format!("Description for {i}"),
            path: format!("skills/skill-{i}/SKILL.md"),
            source: SkillSource::Session,
        })
        .collect();

    // Query has zero overlap with any skill
    let catalog = render_skills_catalog(&skills, Some("quantum physics")).unwrap();
    // All 6 descriptions should be present (full display fallback)
    for i in 0..6 {
        assert!(
            catalog.contains(&format!("Description for {i}")),
            "skill-{i} description should be shown on zero-hit fallback"
        );
    }
}

#[test]
fn skill_source_label_returns_correct_strings() {
    assert_eq!(SkillSource::System.label(), "system");
    assert_eq!(SkillSource::Global.label(), "global");
    assert_eq!(SkillSource::Session.label(), "session");
}

#[test]
fn ensure_session_workspace_creates_skills_directory() {
    let workspace = std::env::temp_dir().join("lingclaw-ensure-skills-dir-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(&workspace).expect("workspace should be created");

    ensure_session_workspace(&workspace);

    assert!(workspace.join("skills").is_dir());

    let _ = fs::remove_dir_all(&workspace);
}

// ── is_system_skill_disabled tests ───────────────────────────────────────────────────────

#[test]
fn system_skill_disabled_exact_match() {
    let disabled = HashSet::from(["anthropics/pdf".to_string()]);
    assert!(is_system_skill_disabled(
        "system://skills/anthropics/pdf/SKILL.md",
        &disabled,
    ));
}

#[test]
fn system_skill_disabled_group_match() {
    let disabled = HashSet::from(["anthropics".to_string()]);
    assert!(is_system_skill_disabled(
        "system://skills/anthropics/pdf/SKILL.md",
        &disabled,
    ));
    assert!(is_system_skill_disabled(
        "system://skills/anthropics/xlsx/SKILL.md",
        &disabled,
    ));
}

#[test]
fn system_skill_disabled_no_match() {
    let disabled = HashSet::from(["anthropics/pdf".to_string()]);
    assert!(!is_system_skill_disabled(
        "system://skills/anthropics/xlsx/SKILL.md",
        &disabled,
    ));
}

#[test]
fn system_skill_disabled_empty_set() {
    let disabled = HashSet::new();
    assert!(!is_system_skill_disabled(
        "system://skills/anthropics/pdf/SKILL.md",
        &disabled,
    ));
}

#[test]
fn system_skill_disabled_no_partial_prefix_collision() {
    // "anthro" should NOT match "anthropics/pdf"
    let disabled = HashSet::from(["anthro".to_string()]);
    assert!(!is_system_skill_disabled(
        "system://skills/anthropics/pdf/SKILL.md",
        &disabled,
    ));
}

#[test]
fn system_skill_disabled_non_system_path_passthrough() {
    let disabled = HashSet::from(["anthropics".to_string()]);
    // Paths without system:// prefix still work (rel_dir fallback)
    assert!(!is_system_skill_disabled(
        "global://skills/other/SKILL.md",
        &disabled,
    ));
}

#[test]
fn load_session_prompt_files_truncates_large_daily_memory() {
    let workspace = std::env::temp_dir().join("lingclaw-prompt-daily-trunc-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(workspace.join("memory")).expect("memory dir");
    fs::write(workspace.join("AGENTS.md"), "agent").unwrap();
    fs::write(workspace.join("IDENTITY.md"), "identity").unwrap();
    fs::write(workspace.join("USER.md"), "user").unwrap();

    // Write a daily memory file that exceeds the 4000-char budget.
    let big_content = "x".repeat(6000);
    fs::write(workspace.join("memory/2026-03-16.md"), &big_content).unwrap();

    let snapshot = LocalTimeSnapshot::from_datetime(
        DateTime::parse_from_rfc3339("2026-03-16T12:00:00+08:00").unwrap(),
    );
    let loaded = load_session_prompt_files_with_snapshot(&workspace, snapshot);

    // The injected daily memory must not exceed the budget + truncation marker.
    let daily_marker = "<!-- memory/2026-03-16.md -->";
    let daily_start = loaded
        .find(daily_marker)
        .expect("daily memory should be present");
    let daily_section = &loaded[daily_start..];
    // Budget is 4000 chars; with truncation marker overhead, the section should
    // be well under 4200 chars and certainly below the original 6000.
    assert!(
        daily_section.len() < 4200,
        "daily memory section should be truncated, got {} chars",
        daily_section.len()
    );
    assert!(
        daily_section.contains("truncated"),
        "truncated daily memory should contain truncation marker"
    );

    let _ = fs::remove_dir_all(&workspace);
}

// ── Cache invalidation regression tests ──────────────────────────────────────

#[test]
fn collect_dir_tree_mtimes_tracks_subdirectories() {
    let dir = std::env::temp_dir().join("lingclaw-dir-tree-mtime-test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("alpha")).expect("alpha subdir");
    fs::create_dir_all(dir.join("beta")).expect("beta subdir");
    // Non-directory entries should be ignored
    fs::write(dir.join("README.md"), "ignore me").unwrap();

    let mtimes = collect_dir_tree_mtimes(&dir);
    // Root + 2 subdirectories = 3 entries
    assert_eq!(mtimes.len(), 3, "root + 2 subdirs");
    assert!(
        mtimes.iter().all(|m| m.is_some()),
        "all existing dirs should have mtimes"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn collect_dir_tree_mtimes_nonexistent_returns_single_none() {
    let dir = std::env::temp_dir().join("lingclaw-dir-tree-mtime-nonexist");
    let _ = fs::remove_dir_all(&dir);

    let mtimes = collect_dir_tree_mtimes(&dir);
    assert_eq!(mtimes, vec![None]);
}

#[test]
fn collect_dir_tree_mtimes_detects_new_subdirectory() {
    let dir = std::env::temp_dir().join("lingclaw-dir-tree-mtime-detect-new");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("root dir");

    let before = collect_dir_tree_mtimes(&dir);

    // Add a new subdirectory — vector length changes
    fs::create_dir_all(dir.join("new-skill")).expect("new skill subdir");
    let after = collect_dir_tree_mtimes(&dir);

    assert_ne!(
        before, after,
        "adding a subdirectory should change the mtime vector"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn invalidate_skills_cache_forces_rediscovery() {
    let workspace = std::env::temp_dir().join("lingclaw-skills-cache-inval-test");
    let _ = fs::remove_dir_all(&workspace);
    let skills_dir = workspace.join("skills");

    // Setup: one skill
    let skill_a = skills_dir.join("alpha");
    fs::create_dir_all(&skill_a).expect("skill dir");
    fs::write(
        skill_a.join("SKILL.md"),
        "---\nname: alpha\ndescription: First\n---\n",
    )
    .unwrap();

    // Prime the cache
    invalidate_skills_cache(); // clear any stale global state from other tests
    let first = discover_all_skills(&workspace);
    let alpha_count = first.iter().filter(|s| s.name == "alpha").count();
    assert!(alpha_count >= 1, "alpha should be discovered");

    // Modify the skill content (mtime of SKILL.md changes, but dir mtime
    // doesn't change on content-only edits on some OSes). Force-invalidate.
    fs::write(
        skill_a.join("SKILL.md"),
        "---\nname: alpha\ndescription: Updated\n---\n",
    )
    .unwrap();
    invalidate_skills_cache();

    let second = discover_all_skills(&workspace);
    let updated = second.iter().find(|s| s.name == "alpha").unwrap();
    assert_eq!(
        updated.description, "Updated",
        "post-invalidation should pick up new content"
    );

    let _ = fs::remove_dir_all(&workspace);
}

#[test]
fn prompt_cache_invalidates_on_file_change() {
    let workspace = std::env::temp_dir().join("lingclaw-prompt-cache-inval-test");
    let _ = fs::remove_dir_all(&workspace);
    fs::create_dir_all(workspace.join("memory")).unwrap();
    fs::write(workspace.join("AGENTS.md"), "agent").unwrap();
    fs::write(workspace.join("IDENTITY.md"), "identity-v1").unwrap();
    fs::write(workspace.join("USER.md"), "user").unwrap();

    let snapshot = LocalTimeSnapshot::from_datetime(
        DateTime::parse_from_rfc3339("2026-03-16T12:00:00+08:00").unwrap(),
    );
    let first = load_session_prompt_files_with_snapshot(&workspace, snapshot);
    assert!(first.contains("identity-v1"));

    // Modify IDENTITY.md — mtime changes → cache miss
    fs::write(workspace.join("IDENTITY.md"), "identity-v2").unwrap();
    let second = load_session_prompt_files_with_snapshot(&workspace, snapshot);
    assert!(
        second.contains("identity-v2"),
        "changed file should invalidate prompt cache"
    );

    let _ = fs::remove_dir_all(&workspace);
}
