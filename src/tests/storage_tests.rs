use super::*;

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_home(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "lingclaw-storage-{label}-{}-{nonce}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn remove_home(path: &Path) {
    let _ = std::fs::remove_dir_all(path);
}

fn chat_message(role: &str, content: &str, timestamp: u64) -> crate::ChatMessage {
    crate::ChatMessage {
        role: role.to_string(),
        content: Some(content.to_string()),
        images: None,
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: None,
        timestamp: Some(timestamp),
    }
}

fn pending_plan_progress(id: &str, title: &str) -> Vec<crate::plan::PlanProgressStep> {
    vec![crate::plan::PlanProgressStep {
        id: id.to_string(),
        title: title.to_string(),
        ..Default::default()
    }]
}

fn basic_session(id: &str, name: &str) -> crate::Session {
    crate::Session {
        id: id.to_string(),
        name: name.to_string(),
        messages: vec![chat_message("system", "system prompt", 1)],
        created_at: 10,
        updated_at: 20,
        tool_calls_count: 0,
        input_tokens: 0,
        output_tokens: 0,
        daily_input_tokens: 0,
        daily_output_tokens: 0,
        input_token_source: crate::default_token_usage_source(),
        output_token_source: crate::default_token_usage_source(),
        token_usage_day: "2026-07-19".to_string(),
        daily_provider_usage: HashMap::new(),
        total_label_usage: HashMap::new(),
        usage_history: Vec::new(),
        model_override: None,
        think_level: crate::default_think_level(),
        show_react: true,
        show_tools: true,
        show_reasoning: true,
        enabled_system_skills: HashSet::new(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: HashSet::new(),
        subagent_snapshots: HashMap::new(),
        todos: crate::todos::TodoSnapshot::empty(20),
        pending_plan: None,
        version: crate::SESSION_VERSION,
        workspace: PathBuf::new(),
    }
}

fn populated_session() -> crate::Session {
    let mut session = basic_session("worker-a", "Worker A");
    session.created_at = 100;
    session.updated_at = 900;
    session.tool_calls_count = 1;
    session.input_tokens = 1200;
    session.output_tokens = 340;
    session.daily_input_tokens = 120;
    session.daily_output_tokens = 34;
    session.input_token_source = "provider".to_string();
    session.output_token_source = "estimated".to_string();
    session.model_override = Some("openai/gpt-vision".to_string());
    session.think_level = "high".to_string();
    session.show_react = false;
    session.show_reasoning = false;
    session.enabled_system_skills =
        HashSet::from(["anthropics".to_string(), "anthropics/pdf".to_string()]);
    session
        .messages
        .push(chat_message("user", "inspect image", 101));
    session.messages.push(crate::ChatMessage {
        role: "assistant".to_string(),
        content: Some("I will inspect it.".to_string()),
        images: None,
        thinking: Some("Use the image tool.".to_string()),
        anthropic_thinking_blocks: Some(vec![crate::AnthropicThinkingBlock {
            block_type: "thinking".to_string(),
            thinking: Some("signed thought".to_string()),
            signature: Some("signature".to_string()),
            data: None,
        }]),
        tool_calls: Some(vec![crate::ToolCall {
            id: "call-image".to_string(),
            call_type: "function".to_string(),
            gemini_thought_signature: Some("gemini-signature".to_string()),
            function: crate::FunctionCall {
                name: "view_image".to_string(),
                arguments: r#"{"path":"diagram.png"}"#.to_string(),
            },
        }]),
        tool_call_id: None,
        timestamp: Some(102),
    });
    session.messages.push(crate::ChatMessage {
        role: "tool".to_string(),
        content: Some("image available".to_string()),
        images: Some(vec![crate::ImageAttachment {
            url: "https://signed.example.invalid/private-image-marker".to_string(),
            name: Some("diagram.png".to_string()),
            mime_type: Some("image/png".to_string()),
            s3_object_key: Some("tools/diagram.png".to_string()),
            s3_config_id: Some("s3-config-a".to_string()),
            cache_path: Some(".lingclaw-cache/diagram.b64".to_string()),
            data: Some("base64-private-image-marker".to_string()),
        }]),
        thinking: None,
        anthropic_thinking_blocks: None,
        tool_calls: None,
        tool_call_id: Some("call-image".to_string()),
        timestamp: Some(103),
    });
    session.failed_tool_results =
        HashSet::from(["call-image".to_string(), "stale-call".to_string()]);
    session.subagent_snapshots.insert(
        "call-image@1".to_string(),
        crate::SubagentHistorySnapshot {
            reasoning: Some("inspected pixels".to_string()),
            cycles: 2,
            tool_calls: 1,
            duration_ms: 42,
            input_tokens: 12,
            output_tokens: 8,
            success: true,
            result_excerpt: Some("a diagram".to_string()),
            ..Default::default()
        },
    );
    session.todos = crate::todos::TodoSnapshot {
        revision: 4,
        items: vec![
            crate::todos::TodoItem {
                id: "todo-a".to_string(),
                content: "Inspect diagram".to_string(),
                status: crate::todos::TodoStatus::Completed,
            },
            crate::todos::TodoItem {
                id: "todo-b".to_string(),
                content: "Write summary".to_string(),
                status: crate::todos::TodoStatus::InProgress,
            },
        ],
        last_updated_by: crate::todos::TodoUpdatedBy::User,
        updated_at: 800,
    };
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan-a".to_string(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        created_at: 700,
        revision: 2,
        status: crate::plan::PlanStatus::Stopped,
        artifact: crate::plan::PlanArtifact {
            schema_version: 1,
            title: "Inspect and summarize".into(),
            goal: "Produce a verified image summary".into(),
            summary: "Use the configured image workflow.".into(),
            steps: vec![crate::plan::PlanStep {
                id: "inspect".into(),
                title: "Inspect the image".into(),
                description: "Read the image through view_image.".into(),
                affected_areas: vec!["diagram.png".into()],
            }],
            verification: vec!["Confirm the summary matches the image.".into()],
            acceptance_criteria: vec!["The result names the visible components.".into()],
            ..Default::default()
        },
        progress: vec![
            crate::plan::PlanProgressStep {
                id: "inspect".into(),
                title: "Inspect the image".into(),
                status: crate::plan::PlanStepStatus::Completed,
                note: "Image inspected".into(),
                deviation_reason: None,
            },
            crate::plan::PlanProgressStep {
                id: "adapt".into(),
                title: "Retry the unavailable preview".into(),
                status: crate::plan::PlanStepStatus::Blocked,
                note: "S3 identity changed".into(),
                deviation_reason: Some("The original signed URL expired".into()),
            },
        ],
        evidence: vec![crate::plan::PlanEvidence {
            path: "diagram.png".into(),
            kind: crate::plan::PlanEvidenceKind::File,
            fingerprint: "sha256-fixture".into(),
            selector: None,
        }],
        evidence_truncated: true,
        updated_at: 710,
        approved_at: Some(705),
        finished_at: Some(710),
        execution_attempt: 1,
        stale_override_paths: vec!["diagram.png".into()],
        stale_override_confirmed_at: Some(706),
        pending_feedback: Some("Keep the storage recovery path explicit.".into()),
        initial_submission_pending: false,
    });
    session
        .daily_provider_usage
        .insert(crate::context::usage_provider_label("openai"), [120, 34]);
    session
        .total_label_usage
        .insert(crate::context::usage_role_label("primary"), [1200, 340]);
    session.usage_history.push(crate::DailyUsageSnapshot {
        date: "2026-07-18".to_string(),
        input: 80,
        output: 21,
        providers: HashMap::from([(crate::context::usage_provider_label("openai"), [80, 21])]),
    });
    session
}

async fn open_temp_database(label: &str) -> (PathBuf, Database) {
    let home = temp_home(label);
    std::fs::create_dir_all(&home).expect("temporary home should be created");
    let database = Database::open(home.join("lingclaw.db"))
        .await
        .expect("database should open");
    (home, database)
}

fn create_current_database(path: &Path) {
    let connection = rusqlite::Connection::open(path).expect("test database should open");
    connection
        .execute_batch(schema::INITIAL_SCHEMA)
        .expect("current schema should initialize");
    connection
        .execute_batch(
            r#"
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (1, 'initial_core_storage', 1);
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (2, 'plan_lifecycle', 1);
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (3, 'durable_plan_feedback', 1);
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (4, 'plan_initial_submission_marker', 1);
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (5, 'plan_stale_override_audit', 1);
            "#,
        )
        .expect("migration ledger should initialize");
    connection
        .pragma_update(None, "application_id", schema::APPLICATION_ID)
        .expect("application id should initialize");
    connection
        .pragma_update(None, "user_version", schema::SCHEMA_VERSION)
        .expect("schema version should initialize");
}

fn create_v1_plan_database(path: &Path, include_assistant_message: bool) {
    let connection = rusqlite::Connection::open(path).expect("test database should open");
    connection
        .execute_batch(schema::INITIAL_SCHEMA)
        .expect("base schema should initialize");
    connection
        .execute_batch(
            r#"
            DROP TABLE session_plan_progress;
            DROP TABLE session_plan_revisions;
            DROP TABLE session_plans;
            CREATE TABLE session_pending_plans (
                session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                plan_id TEXT NOT NULL,
                original_user_message_index INTEGER NOT NULL,
                assistant_plan_message_index INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );
            "#,
        )
        .expect("v1 plan table should initialize");
    connection
        .execute(
            r#"INSERT INTO sessions(
                id, name, created_at, updated_at, tool_calls_count, model_override,
                think_level, show_react, show_tools, show_reasoning,
                visible_message_count, version
            ) VALUES ('legacy-plan-session', 'Legacy plan', 1, 2, 0, NULL,
                      'medium', 1, 1, 1, 2, 7)"#,
            [],
        )
        .expect("session should insert");
    if include_assistant_message {
        connection
            .execute(
                r#"INSERT INTO session_messages(
                    session_id, position, role, content, images_json, thinking,
                    thinking_blocks_json, tool_calls_json, tool_call_id, timestamp, fingerprint
                ) VALUES ('legacy-plan-session', 1, 'assistant', '# Legacy plan\n\n1. Inspect',
                          NULL, NULL, NULL, NULL, NULL, 2, 'fixture')"#,
                [],
            )
            .expect("assistant message should insert");
    }
    connection
        .execute(
            r#"INSERT INTO session_pending_plans(
                session_id, plan_id, original_user_message_index,
                assistant_plan_message_index, created_at
            ) VALUES ('legacy-plan-session', 'legacy-plan-id', 0, 1, 2)"#,
            [],
        )
        .expect("legacy plan should insert");
    connection
        .execute(
            "INSERT INTO schema_migrations(version, name, applied_at) VALUES (1, 'initial_core_storage', 1)",
            [],
        )
        .expect("v1 migration ledger should initialize");
    connection
        .pragma_update(None, "application_id", schema::APPLICATION_ID)
        .expect("application id should initialize");
    connection
        .pragma_update(None, "user_version", 1)
        .expect("schema version should initialize");
}

fn create_v2_plan_database(path: &Path) {
    let connection = rusqlite::Connection::open(path).expect("test database should open");
    connection
        .execute_batch(schema::INITIAL_SCHEMA)
        .expect("base schema should initialize");
    connection
        .execute_batch(
            r#"
            ALTER TABLE session_plans DROP COLUMN initial_submission_pending;
            ALTER TABLE session_plans DROP COLUMN pending_feedback;
            ALTER TABLE session_plans DROP COLUMN stale_override_confirmed_at;
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (1, 'initial_core_storage', 1);
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (2, 'plan_lifecycle', 1);
            "#,
        )
        .expect("v2 plan schema should initialize");
    connection
        .pragma_update(None, "application_id", schema::APPLICATION_ID)
        .expect("application id should initialize");
    connection
        .pragma_update(None, "user_version", 2)
        .expect("schema version should initialize");
}

fn create_v3_plan_database(path: &Path) {
    let connection = rusqlite::Connection::open(path).expect("test database should open");
    connection
        .execute_batch(schema::INITIAL_SCHEMA)
        .expect("base schema should initialize");
    connection
        .execute_batch(
            r#"
            ALTER TABLE session_plans DROP COLUMN initial_submission_pending;
            ALTER TABLE session_plans DROP COLUMN stale_override_confirmed_at;
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (1, 'initial_core_storage', 1);
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (2, 'plan_lifecycle', 1);
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (3, 'durable_plan_feedback', 1);
            "#,
        )
        .expect("v3 plan schema should initialize");
    connection
        .pragma_update(None, "application_id", schema::APPLICATION_ID)
        .expect("application id should initialize");
    connection
        .pragma_update(None, "user_version", 3)
        .expect("schema version should initialize");
}

fn create_v4_plan_database(path: &Path) {
    let connection = rusqlite::Connection::open(path).expect("test database should open");
    connection
        .execute_batch(schema::INITIAL_SCHEMA)
        .expect("base schema should initialize");
    connection
        .execute_batch(
            r#"
            ALTER TABLE session_plans DROP COLUMN stale_override_confirmed_at;
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (1, 'initial_core_storage', 1);
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (2, 'plan_lifecycle', 1);
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (3, 'durable_plan_feedback', 1);
            INSERT INTO schema_migrations(version, name, applied_at)
                VALUES (4, 'plan_initial_submission_marker', 1);
            "#,
        )
        .expect("v4 plan schema should initialize");
    connection
        .pragma_update(None, "application_id", schema::APPLICATION_ID)
        .expect("application id should initialize");
    connection
        .pragma_update(None, "user_version", 4)
        .expect("schema version should initialize");
}

fn migration_journal_fixture(
    home: &Path,
    suffix: &str,
    phase: &str,
    session_id: &str,
    session_name: &str,
) -> (PathBuf, serde_json::Value) {
    let backup = home.join(format!("backups/sqlite-migration-{suffix}"));
    let backup_sessions = backup.join("sessions");
    std::fs::create_dir_all(&backup_sessions).unwrap();
    let session = basic_session(session_id, session_name);
    let bytes = serde_json::to_vec_pretty(&session).unwrap();
    std::fs::write(backup_sessions.join(format!("{session_id}.json")), &bytes).unwrap();
    let journal = serde_json::json!({
        "version": 1,
        "phase": phase,
        "backup_dir": backup,
        "had_sessions_dir": true,
        "had_groups_dir": false,
        "manifest": {
            "version": 1,
            "created_at": 1,
            "sessions": 1,
            "groups": 0,
            "files": [{
                "kind": "session",
                "id": session_id,
                "source": format!("sessions/{session_id}.json"),
                "sha256": format!("{:x}", Sha256::digest(&bytes)),
            }],
        },
    });
    (backup, journal)
}

#[tokio::test]
async fn opens_an_empty_database_with_the_current_schema() {
    let (home, database) = open_temp_database("open").await;
    assert_eq!(database.status().mode, StorageMode::Healthy);
    assert_eq!(database.path(), home.join("lingclaw.db").as_path());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        for path in [
            database.path().to_path_buf(),
            PathBuf::from(format!("{}-wal", database.path().display())),
            PathBuf::from(format!("{}-shm", database.path().display())),
        ] {
            if path.exists() {
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600,
                    "{} should be private",
                    path.display()
                );
            }
        }
    }
    let (application_id, version, foreign_keys, journal_mode, tables) = database
        .read(|connection| {
            let application_id =
                connection.query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))?;
            let version =
                connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
            let foreign_keys =
                connection.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?;
            let journal_mode =
                connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?;
            let mut statement =
                connection.prepare("SELECT name FROM sqlite_master WHERE type='table'")?;
            let tables = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<HashSet<_>, _>>()?;
            Ok((application_id, version, foreign_keys, journal_mode, tables))
        })
        .await
        .expect("schema metadata should be readable");
    assert_eq!(application_id, schema::APPLICATION_ID);
    assert_eq!(version, schema::SCHEMA_VERSION);
    assert_eq!(foreign_keys, 1);
    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    for required in [
        "sessions",
        "session_messages",
        "session_todos",
        "session_usage",
        "groups",
        "group_members",
        "group_votes",
        "group_messages",
        "group_runs",
        "storage_metadata",
    ] {
        assert!(tables.contains(required), "missing table {required}");
    }
    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn schema_v1_migration_backs_up_and_converts_legacy_pending_plans() {
    let home = temp_home("schema-v1-plan");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join("lingclaw.db");
    create_v1_plan_database(&path, true);

    let database = Database::open(path.clone())
        .await
        .expect("v1 database should migrate");
    let converted = database
        .read(|connection| {
            let version =
                connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
            let plan = connection.query_row(
                r#"SELECT p.status, p.current_revision, r.artifact_json
                   FROM session_plans p
                   JOIN session_plan_revisions r
                     ON r.session_id=p.session_id AND r.plan_id=p.plan_id
                    AND r.revision=p.current_revision
                   WHERE p.session_id='legacy-plan-session'"#,
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?;
            let legacy_table: i64 = connection.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_pending_plans'",
                [],
                |row| row.get(0),
            )?;
            Ok((version, plan, legacy_table))
        })
        .await
        .expect("converted plan should be queryable");
    assert_eq!(converted.0, schema::SCHEMA_VERSION);
    assert_eq!(converted.1.0, "ready");
    assert_eq!(converted.1.1, 1);
    assert!(converted.1.2.contains("# Legacy plan"));
    assert_eq!(converted.2, 0);

    let backups = std::fs::read_dir(home.join("backups"))
        .expect("schema backup directory should exist")
        .collect::<Result<Vec<_>, _>>()
        .expect("schema backups should be readable");
    assert!(backups.iter().any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .starts_with("lingclaw-schema-v1-")
    }));

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn schema_v1_migration_preserves_legacy_plans_above_the_new_submission_limit() {
    let home = temp_home("schema-v1-large-plan");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join("lingclaw.db");
    create_v1_plan_database(&path, true);
    let markdown = format!(
        "# Large legacy plan\n\n{}",
        "x".repeat(crate::plan::MAX_PLAN_BYTES / 2)
    );
    assert!(
        serde_json::to_vec(&crate::plan::legacy_artifact(&markdown))
            .expect("legacy artifact should serialize")
            .len()
            > crate::plan::MAX_PLAN_BYTES,
        "fixture must exceed the limit applied to newly submitted plans"
    );
    {
        let connection = rusqlite::Connection::open(&path).expect("v1 database should reopen");
        connection
            .execute(
                "UPDATE session_messages SET content=?1 WHERE session_id='legacy-plan-session' AND position=1",
                [&markdown],
            )
            .expect("legacy plan message should update");
    }

    let database = Database::open(path)
        .await
        .expect("an oversized v1 plan must not block schema migration");
    let artifact_json = database
        .read(|connection| {
            connection
                .query_row(
                    "SELECT artifact_json FROM session_plan_revisions WHERE session_id='legacy-plan-session' AND revision=1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .map_err(StorageError::from)
        })
        .await
        .expect("migrated artifact should be readable");
    let artifact: crate::plan::PlanArtifact =
        serde_json::from_str(&artifact_json).expect("migrated artifact should deserialize");
    crate::plan::validate_persisted_legacy_artifact(&artifact)
        .expect("migrated artifact should pass durable validation");
    assert_eq!(artifact.legacy_markdown.as_deref(), Some(markdown.as_str()));
    assert_eq!(artifact.steps[0].description, markdown);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn schema_v2_migration_adds_durable_plan_feedback() {
    let home = temp_home("schema-v2-plan-feedback");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join("lingclaw.db");
    create_v2_plan_database(&path);

    let database = Database::open(path.clone())
        .await
        .expect("v2 database should migrate");
    let (version, columns, migrations) = database
        .read(|connection| {
            let version =
                connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
            let columns = connection
                .prepare("PRAGMA table_info(session_plans)")?
                .query_map([], |row| row.get::<_, String>(1))?
                .collect::<Result<Vec<_>, _>>()?;
            let migrations = connection
                .prepare("SELECT version, name FROM schema_migrations ORDER BY version")?
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok((version, columns, migrations))
        })
        .await
        .expect("migrated schema should be queryable");
    assert_eq!(version, schema::SCHEMA_VERSION);
    assert!(columns.iter().any(|column| column == "pending_feedback"));
    assert!(
        columns
            .iter()
            .any(|column| column == "initial_submission_pending")
    );
    assert!(
        columns
            .iter()
            .any(|column| column == "stale_override_confirmed_at")
    );
    assert_eq!(
        migrations,
        vec![
            (1, "initial_core_storage".to_string()),
            (2, "plan_lifecycle".to_string()),
            (3, "durable_plan_feedback".to_string()),
            (4, "plan_initial_submission_marker".to_string()),
            (5, "plan_stale_override_audit".to_string()),
        ]
    );

    let backups = std::fs::read_dir(home.join("backups"))
        .expect("schema backup directory should exist")
        .collect::<Result<Vec<_>, _>>()
        .expect("schema backups should be readable");
    assert!(backups.iter().any(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .starts_with("lingclaw-schema-v2-")
    }));

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn schema_v3_migration_adds_initial_submission_marker() {
    let home = temp_home("schema-v3-initial-plan-marker");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join("lingclaw.db");
    create_v3_plan_database(&path);

    let database = Database::open(path.clone())
        .await
        .expect("v3 database should migrate");
    let (version, marker_default, migration_name) = database
        .read(|connection| {
            let version =
                connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
            let marker_default = connection
                .prepare("PRAGMA table_info(session_plans)")?
                .query_map([], |row| {
                    Ok((row.get::<_, String>(1)?, row.get::<_, Option<String>>(4)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .find(|(name, _)| name == "initial_submission_pending")
                .and_then(|(_, default)| default);
            let migration_name = connection.query_row(
                "SELECT name FROM schema_migrations WHERE version=4",
                [],
                |row| row.get::<_, String>(0),
            )?;
            Ok((version, marker_default, migration_name))
        })
        .await
        .expect("migrated marker should be queryable");
    assert_eq!(version, schema::SCHEMA_VERSION);
    assert_eq!(marker_default.as_deref(), Some("0"));
    assert_eq!(migration_name, "plan_initial_submission_marker");

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn schema_v4_migration_adds_plan_stale_override_audit() {
    let home = temp_home("schema-v4-stale-override-audit");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join("lingclaw.db");
    create_v4_plan_database(&path);

    let database = Database::open(path.clone())
        .await
        .expect("v4 database should migrate");
    let (version, override_default, migration_name) = database
        .read(|connection| {
            let version =
                connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
            let override_default = connection
                .prepare("PRAGMA table_info(session_plans)")?
                .query_map([], |row| {
                    Ok((row.get::<_, String>(1)?, row.get::<_, Option<String>>(4)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .find(|(name, _)| name == "stale_override_confirmed_at")
                .and_then(|(_, default)| default);
            let migration_name = connection.query_row(
                "SELECT name FROM schema_migrations WHERE version=5",
                [],
                |row| row.get::<_, String>(0),
            )?;
            Ok((version, override_default, migration_name))
        })
        .await
        .expect("migrated audit field should be queryable");
    assert_eq!(version, schema::SCHEMA_VERSION);
    assert_eq!(override_default, None);
    assert_eq!(migration_name, "plan_stale_override_audit");

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn schema_migration_rolls_back_when_precommit_validation_finds_drift() {
    let home = temp_home("schema-drift-rollback");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join("lingclaw.db");
    create_v2_plan_database(&path);
    {
        let connection = rusqlite::Connection::open(&path).expect("fixture should open");
        connection
            .execute("CREATE TABLE unexpected_extension(value TEXT)", [])
            .expect("schema drift should be installed");
    }

    let error = match Database::open(path.clone()).await {
        Ok(_) => panic!("schema drift must abort migration"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("unexpected_extension"));

    let connection = rusqlite::Connection::open(&path).expect("original database should remain");
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    let columns = connection
        .prepare("PRAGMA table_info(session_plans)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let migration_three: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version=3",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, 2);
    assert!(!columns.iter().any(|column| column == "pending_feedback"));
    assert!(
        !columns
            .iter()
            .any(|column| column == "initial_submission_pending")
    );
    assert_eq!(migration_three, 0);
    drop(connection);
    remove_home(&home);
}

#[tokio::test]
async fn schema_v1_plan_migration_rolls_back_when_legacy_message_is_missing() {
    let home = temp_home("schema-v1-plan-rollback");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join("lingclaw.db");
    create_v1_plan_database(&path, false);

    let error = match Database::open(path.clone()).await {
        Ok(_) => panic!("invalid legacy plan must abort migration"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("missing assistant message"));

    let connection = rusqlite::Connection::open(&path).expect("original database should remain");
    let version = connection
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .unwrap();
    let legacy_table: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_pending_plans'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let lifecycle_table: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='session_plans'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, 1);
    assert_eq!(legacy_table, 1);
    assert_eq!(lifecycle_table, 0);
    drop(connection);

    remove_home(&home);
}

#[tokio::test]
async fn refuses_to_repair_a_damaged_current_schema() {
    for (label, damage, expected) in [
        (
            "missing-table",
            "DROP TABLE session_messages",
            "session_messages",
        ),
        (
            "missing-index",
            "DROP INDEX idx_groups_updated_at",
            "missing: idx_groups_updated_at",
        ),
        (
            "unexpected-trigger",
            "CREATE TRIGGER unexpected_session_trigger \
             AFTER INSERT ON sessions BEGIN \
                 UPDATE sessions SET name=name WHERE id=NEW.id; \
             END",
            "unexpected: unexpected_session_trigger",
        ),
        (
            "unexpected-view",
            "CREATE VIEW unexpected_session_view AS SELECT id FROM sessions",
            "unexpected: unexpected_session_view",
        ),
        (
            "missing-ledger",
            "DELETE FROM schema_migrations",
            "migration ledger",
        ),
    ] {
        let home = temp_home(label);
        std::fs::create_dir_all(&home).unwrap();
        let path = home.join("lingclaw.db");
        create_current_database(&path);
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection.execute_batch(damage).unwrap();
        }

        let error = Database::open(path.clone())
            .await
            .err()
            .expect("damaged current schema must be rejected");
        assert!(
            error.to_string().contains(expected),
            "unexpected error: {error}"
        );

        if label == "missing-table" {
            let connection = rusqlite::Connection::open(&path).unwrap();
            let table_exists = connection
                .query_row(
                    "SELECT 1 FROM sqlite_master WHERE type='table' AND name='session_messages'",
                    [],
                    |_| Ok(()),
                )
                .optional()
                .unwrap()
                .is_some();
            assert!(
                !table_exists,
                "startup must not hide damage by recreating the missing table"
            );
        }
        remove_home(&home);
    }
}

#[tokio::test]
async fn refuses_foreign_or_newer_database_headers_without_rewriting_them() {
    for (label, application_id, version, expected) in [
        ("foreign", 0x1234_i64, 0_i64, "does not belong to LingClaw"),
        (
            "future",
            schema::APPLICATION_ID,
            schema::SCHEMA_VERSION + 1,
            "newer than this binary",
        ),
    ] {
        let home = temp_home(label);
        std::fs::create_dir_all(&home).unwrap();
        let path = home.join("lingclaw.db");
        {
            let connection = rusqlite::Connection::open(&path).unwrap();
            connection
                .pragma_update(None, "application_id", application_id)
                .unwrap();
            connection
                .pragma_update(None, "user_version", version)
                .unwrap();
        }

        let error = Database::open(path.clone())
            .await
            .err()
            .expect("database header should be rejected");
        assert!(error.to_string().contains(expected));
        let connection = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            application_id
        );
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            version
        );
        drop(connection);
        remove_home(&home);
    }
}

#[tokio::test]
async fn refuses_unidentified_nonempty_database_without_claiming_it() {
    let home = temp_home("unidentified-nonempty");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join("lingclaw.db");
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE foreign_data(value TEXT NOT NULL);
                 INSERT INTO foreign_data(value) VALUES ('keep-me');",
            )
            .unwrap();
    }

    let error = Database::open(path.clone())
        .await
        .err()
        .expect("an unidentified nonempty database must be rejected");
    assert!(error.to_string().contains("not empty"));

    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT value FROM foreign_data", [], |row| {
                row.get::<_, String>(0)
            })
            .unwrap(),
        "keep-me"
    );
    drop(connection);
    remove_home(&home);
}

#[tokio::test]
async fn session_round_trip_preserves_fields_without_ephemeral_image_secrets() {
    let (home, database) = open_temp_database("session-round-trip").await;
    let session = populated_session();
    let expected =
        crate::session_store::session_for_storage(&session).expect("session should be sanitizable");

    database
        .save_session(&session)
        .await
        .expect("session should save");
    let loaded = database
        .load_session(&session.id)
        .await
        .expect("session should load")
        .expect("saved session should exist");

    let mut loaded_json = serde_json::to_value(&loaded).expect("loaded session should serialize");
    let mut expected_json =
        serde_json::to_value(&expected).expect("expected session should serialize");
    for value in [&mut loaded_json, &mut expected_json] {
        for field in ["enabled_system_skills", "failed_tool_results"] {
            if let Some(items) = value
                .get_mut(field)
                .and_then(serde_json::Value::as_array_mut)
            {
                items.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
            }
        }
    }
    assert_eq!(loaded_json, expected_json);
    assert!(loaded.failed_tool_results.contains("call-image"));
    assert!(!loaded.failed_tool_results.contains("stale-call"));
    let stored_image = loaded.messages[3].images.as_ref().unwrap().first().unwrap();
    assert!(stored_image.url.is_empty());
    assert!(stored_image.data.is_none());
    assert_eq!(
        stored_image.s3_object_key.as_deref(),
        Some("tools/diagram.png")
    );

    database
        .checkpoint()
        .await
        .expect("checkpoint should succeed");
    let database_bytes = std::fs::read(database.path()).expect("database file should be readable");
    let searchable = String::from_utf8_lossy(&database_bytes);
    assert!(!searchable.contains("private-image-marker"));
    assert!(!searchable.contains("base64-private-image-marker"));

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn initial_plan_submission_marker_survives_a_database_round_trip() {
    let (home, database) = open_temp_database("initial-plan-marker-round-trip").await;
    let mut session = basic_session("initial-plan-marker", "Initial plan marker");
    session
        .messages
        .push(chat_message("user", "Plan this change", 2));
    let mut plan = crate::PendingPlan::new(
        "plan-initial-marker".into(),
        1,
        1,
        2,
        1,
        crate::plan::PlanStatus::Planning,
        crate::plan::PlanArtifact {
            title: "Planning".into(),
            goal: "Plan this change".into(),
            ..Default::default()
        },
        Vec::new(),
        false,
    );
    plan.initial_submission_pending = true;
    session.pending_plan = Some(plan);

    database.save_session(&session).await.unwrap();
    let loaded = database.load_session(&session.id).await.unwrap().unwrap();

    assert!(
        loaded
            .pending_plan
            .as_ref()
            .is_some_and(|plan| plan.initial_submission_pending)
    );

    let plan = session.pending_plan.as_mut().unwrap();
    plan.initial_submission_pending = false;
    plan.status = crate::plan::PlanStatus::Ready;
    plan.artifact.title = "Final plan".into();
    plan.artifact.steps.push(crate::plan::PlanStep {
        id: "implement".into(),
        title: "Implement the plan".into(),
        ..Default::default()
    });
    plan.evidence.push(crate::plan::PlanEvidence {
        path: "README.md".into(),
        kind: crate::plan::PlanEvidenceKind::File,
        fingerprint: "sha256-final".into(),
        selector: None,
    });
    plan.updated_at = 3;
    database.save_session(&session).await.unwrap();

    let finalized = database.load_session(&session.id).await.unwrap().unwrap();
    let finalized_plan = finalized.pending_plan.unwrap();
    assert!(!finalized_plan.initial_submission_pending);
    assert_eq!(finalized_plan.artifact.title, "Final plan");
    assert_eq!(finalized_plan.evidence.len(), 1);
    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn persisted_plan_revision_rejects_artifact_and_evidence_rewrites() {
    let (home, database) = open_temp_database("immutable-plan-revision").await;
    let mut session = basic_session("immutable-plan-revision", "Immutable plan revision");
    session.pending_plan = Some(crate::PendingPlan {
        id: "immutable-plan".into(),
        revision: 1,
        status: crate::plan::PlanStatus::Ready,
        artifact: crate::plan::PlanArtifact {
            title: "Original plan".into(),
            goal: "Preserve the approved contract".into(),
            steps: vec![crate::plan::PlanStep {
                id: "inspect".into(),
                title: "Inspect".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        updated_at: 2,
        ..Default::default()
    });
    database.save_session(&session).await.unwrap();

    let plan = session.pending_plan.as_mut().unwrap();
    plan.artifact.title = "Rewritten plan".into();
    plan.evidence.push(crate::plan::PlanEvidence {
        path: "Cargo.toml".into(),
        kind: crate::plan::PlanEvidenceKind::File,
        fingerprint: "sha256-rewritten".into(),
        selector: None,
    });
    plan.updated_at = 3;
    let error = database
        .save_session(&session)
        .await
        .expect_err("the same revision must remain immutable");
    assert!(error.to_string().contains("Plan revision is immutable"));

    let stored = database.load_session(&session.id).await.unwrap().unwrap();
    let stored_plan = stored.pending_plan.unwrap();
    assert_eq!(stored_plan.artifact.title, "Original plan");
    assert!(stored_plan.evidence.is_empty());
    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn plan_history_returns_superseded_revisions_as_read_only_entries() {
    let (home, database) = open_temp_database("plan-history").await;
    let mut session = basic_session("plan-history", "Plan history");
    session
        .messages
        .push(chat_message("user", "Plan this change", 2));
    session
        .messages
        .push(chat_message("assistant", "First plan", 3));
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan-history-id".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        created_at: 2,
        revision: 1,
        status: crate::plan::PlanStatus::Ready,
        artifact: crate::plan::PlanArtifact {
            title: "First revision".into(),
            goal: "Ship the change".into(),
            steps: vec![crate::plan::PlanStep {
                id: "step-1".into(),
                title: "Inspect".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        progress: pending_plan_progress("step-1", "Inspect"),
        updated_at: 3,
        ..Default::default()
    });
    database.save_session(&session).await.unwrap();

    session
        .messages
        .push(chat_message("user", "Include verification", 4));
    session
        .messages
        .push(chat_message("assistant", "Second plan", 5));
    let plan = session.pending_plan.as_mut().unwrap();
    plan.revision = 2;
    plan.assistant_plan_message_index = 4;
    plan.artifact.title = "Second revision".into();
    plan.updated_at = 5;
    database.save_session(&session).await.unwrap();

    let history = database.load_plan_history(&session.id).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["revision"], 1);
    assert_eq!(history[0]["artifact"]["title"], "First revision");
    assert_eq!(history[0]["historical"], true);
    assert_eq!(history[1]["revision"], 2);
    assert_eq!(history[1]["artifact"]["title"], "Second revision");
    assert_eq!(history[1]["historical"], false);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn plan_history_preserves_a_superseded_needs_input_revision() {
    let (home, database) = open_temp_database("plan-history-needs-input").await;
    let mut session = basic_session("plan-history-needs-input", "Plan history needs input");
    session
        .messages
        .push(chat_message("user", "Plan this change", 2));
    session
        .messages
        .push(chat_message("assistant", "Which scope?", 3));
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan-history-needs-input-id".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        created_at: 2,
        revision: 1,
        status: crate::plan::PlanStatus::NeedsInput,
        artifact: crate::plan::PlanArtifact {
            title: "Choose scope".into(),
            goal: "Plan the selected scope".into(),
            questions: vec![crate::plan::PlanQuestion {
                id: "scope".into(),
                prompt: "Which scope should the plan cover?".into(),
                options: vec![
                    crate::plan::PlanQuestionOption {
                        id: "focused".into(),
                        label: "Focused".into(),
                        ..Default::default()
                    },
                    crate::plan::PlanQuestionOption {
                        id: "complete".into(),
                        label: "Complete".into(),
                        ..Default::default()
                    },
                ],
            }],
            ..Default::default()
        },
        updated_at: 3,
        ..Default::default()
    });
    database.save_session(&session).await.unwrap();

    session
        .messages
        .push(chat_message("user", "Use the focused scope", 4));
    session
        .messages
        .push(chat_message("assistant", "Focused plan", 5));
    let plan = session.pending_plan.as_mut().unwrap();
    plan.revision = 2;
    plan.status = crate::plan::PlanStatus::Ready;
    plan.assistant_plan_message_index = 4;
    plan.artifact = crate::plan::PlanArtifact {
        title: "Focused plan".into(),
        goal: "Implement the focused change".into(),
        steps: vec![crate::plan::PlanStep {
            id: "implement".into(),
            title: "Implement".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    plan.progress = pending_plan_progress("implement", "Implement");
    plan.updated_at = 5;
    database.save_session(&session).await.unwrap();

    let history = database.load_plan_history(&session.id).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["revision"], 1);
    assert_eq!(history[0]["status"], "needs_input");
    assert_eq!(history[0]["historical"], true);
    assert_eq!(history[1]["revision"], 2);
    assert_eq!(history[1]["status"], "ready");
    assert_eq!(history[1]["historical"], false);
    assert_eq!(database.status().mode, crate::storage::StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn plan_history_is_bounded_and_keeps_the_current_revision() {
    let (home, database) = open_temp_database("bounded-plan-history").await;
    let mut session = basic_session("bounded-plan-history", "Bounded plan history");
    session
        .messages
        .push(chat_message("user", "Plan this change", 2));
    session
        .messages
        .push(chat_message("assistant", "Plan revision", 3));
    session.pending_plan = Some(crate::PendingPlan {
        id: "bounded-plan-id".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        created_at: 2,
        revision: 1,
        status: crate::plan::PlanStatus::Ready,
        artifact: crate::plan::PlanArtifact {
            title: "Revision 1".into(),
            goal: "Keep history bounded".into(),
            steps: vec![crate::plan::PlanStep {
                id: "step-1".into(),
                title: "Inspect".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        progress: pending_plan_progress("step-1", "Inspect"),
        updated_at: 3,
        ..Default::default()
    });

    for revision in 1..=55_u32 {
        let plan = session.pending_plan.as_mut().unwrap();
        plan.revision = revision;
        plan.artifact.title = format!("Revision {revision}");
        plan.updated_at = u64::from(revision) + 2;
        database.save_session(&session).await.unwrap();
    }

    let history = database.load_plan_history(&session.id).await.unwrap();
    assert_eq!(history.len(), 50);
    assert_eq!(
        history.first().and_then(|plan| plan["revision"].as_u64()),
        Some(6)
    );
    assert_eq!(
        history.last().and_then(|plan| plan["revision"].as_u64()),
        Some(55)
    );
    assert_eq!(
        history.last().map(|plan| &plan["historical"]),
        Some(&serde_json::json!(false))
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn plan_message_anchors_follow_context_prefix_pruning() {
    let (home, database) = open_temp_database("plan-anchor-prune").await;
    let mut session = basic_session("plan-anchor-prune", "Plan anchor prune");
    session.messages.extend([
        chat_message("user", "Plan this change", 2),
        chat_message("assistant", "First plan", 3),
        chat_message("user", "Add verification", 4),
        chat_message("assistant", "Second plan", 5),
        chat_message("user", "Keep going", 6),
        chat_message("assistant", "Working", 7),
    ]);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan-anchor-id".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        created_at: 2,
        revision: 1,
        status: crate::plan::PlanStatus::Ready,
        artifact: crate::plan::PlanArtifact {
            title: "First revision".into(),
            goal: "Ship the change".into(),
            steps: vec![crate::plan::PlanStep {
                id: "step-1".into(),
                title: "Inspect".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        progress: pending_plan_progress("step-1", "Inspect"),
        updated_at: 3,
        ..Default::default()
    });
    database.save_session(&session).await.unwrap();

    let plan = session.pending_plan.as_mut().unwrap();
    plan.revision = 2;
    plan.original_user_message_index = 3;
    plan.assistant_plan_message_index = 4;
    plan.artifact.title = "Second revision".into();
    plan.updated_at = 5;
    database.save_session(&session).await.unwrap();

    session.messages.drain(1..3);
    session
        .pending_plan
        .as_mut()
        .unwrap()
        .rebase_message_indices_after_prefix_prune(2);
    database.save_session(&session).await.unwrap();

    let history = database.load_plan_history(&session.id).await.unwrap();
    assert_eq!(history[0]["revision"], 1);
    assert_eq!(history[0]["message_index"], 0);
    assert_eq!(history[1]["revision"], 2);
    assert_eq!(history[1]["message_index"], 2);
    let loaded = database.load_session(&session.id).await.unwrap().unwrap();
    let current = loaded.pending_plan.unwrap();
    assert_eq!(current.original_user_message_index, 1);
    assert_eq!(current.assistant_plan_message_index, 2);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn plan_message_anchors_in_unchanged_prefix_survive_tail_rewrites() {
    let (home, database) = open_temp_database("plan-anchor-tail-rewrite").await;
    let mut session = basic_session("plan-anchor-tail-rewrite", "Plan anchor tail rewrite");
    session.messages.extend([
        chat_message("user", "Plan this change", 2),
        chat_message("assistant", "First plan", 3),
        chat_message("user", "Revise the plan", 4),
        chat_message("assistant", "Second plan", 5),
        chat_message("assistant", "Original tail", 6),
    ]);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan-anchor-tail-id".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        created_at: 2,
        revision: 1,
        status: crate::plan::PlanStatus::Ready,
        artifact: crate::plan::PlanArtifact {
            title: "First revision".into(),
            goal: "Preserve historical anchors".into(),
            steps: vec![crate::plan::PlanStep {
                id: "inspect".into(),
                title: "Inspect".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        progress: pending_plan_progress("inspect", "Inspect"),
        updated_at: 3,
        ..Default::default()
    });
    database.save_session(&session).await.unwrap();

    let plan = session.pending_plan.as_mut().unwrap();
    plan.revision = 2;
    plan.original_user_message_index = 3;
    plan.assistant_plan_message_index = 4;
    plan.artifact.title = "Second revision".into();
    plan.updated_at = 5;
    database.save_session(&session).await.unwrap();

    session.messages[5].content = Some("Rewritten tail".into());
    database.save_session(&session).await.unwrap();

    let history = database.load_plan_history(&session.id).await.unwrap();
    let first = history
        .iter()
        .find(|plan| plan["revision"] == 1)
        .expect("first revision should remain in history");
    let second = history
        .iter()
        .find(|plan| plan["revision"] == 2)
        .expect("second revision should remain in history");
    assert_eq!(first["message_index"], 2);
    assert_eq!(second["message_index"], 4);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn plan_message_anchor_rebase_does_not_reuse_an_identical_prefix_message() {
    let (home, database) = open_temp_database("plan-anchor-repeated-message").await;
    let mut session = basic_session(
        "plan-anchor-repeated-message",
        "Plan anchor repeated message",
    );
    session.messages.extend([
        chat_message("user", "Plan this change", 2),
        chat_message("assistant", "Repeated message", 3),
        chat_message("assistant", "Repeated message", 3),
        chat_message("assistant", "Current plan", 4),
    ]);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan-anchor-repeated-id".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 3,
        created_at: 2,
        revision: 1,
        status: crate::plan::PlanStatus::Ready,
        artifact: crate::plan::PlanArtifact {
            title: "Removed revision".into(),
            goal: "Do not reuse an identical prefix message".into(),
            steps: vec![crate::plan::PlanStep {
                id: "inspect".into(),
                title: "Inspect".into(),
                ..Default::default()
            }],
            ..Default::default()
        },
        progress: pending_plan_progress("inspect", "Inspect"),
        updated_at: 3,
        ..Default::default()
    });
    database.save_session(&session).await.unwrap();

    let plan = session.pending_plan.as_mut().unwrap();
    plan.revision = 2;
    plan.assistant_plan_message_index = 4;
    plan.artifact.title = "Current revision".into();
    plan.updated_at = 4;
    database.save_session(&session).await.unwrap();

    session.messages.remove(3);
    session
        .pending_plan
        .as_mut()
        .unwrap()
        .assistant_plan_message_index = 3;
    database.save_session(&session).await.unwrap();

    let history = database.load_plan_history(&session.id).await.unwrap();
    let removed = history
        .iter()
        .find(|plan| plan["revision"] == 1)
        .expect("removed revision should remain in history");
    let current = history
        .iter()
        .find(|plan| plan["revision"] == 2)
        .expect("current revision should remain in history");
    assert_eq!(removed["message_index"], 0);
    assert_eq!(current["message_index"], 3);

    drop(database);
    remove_home(&home);
}

#[test]
fn runtime_instance_lock_is_exclusive_and_reusable() {
    let home = temp_home("runtime-lock");
    let database_path = home.join("lingclaw.db");
    let first = crate::storage::RuntimeInstanceLock::acquire(&database_path).unwrap();
    let error = crate::storage::RuntimeInstanceLock::acquire(&database_path)
        .err()
        .expect("a second process lock must be rejected");
    assert!(error.to_string().contains("another LingClaw process"));
    drop(first);
    let second = crate::storage::RuntimeInstanceLock::acquire(&database_path)
        .expect("the lock should be reusable after release");
    drop(second);
    remove_home(&home);
}

#[tokio::test]
async fn database_recovers_process_bound_plan_states_after_restart() {
    let (home, database) = open_temp_database("recover-interrupted-plans").await;
    let artifact = crate::plan::PlanArtifact {
        title: "Restart-safe plan".into(),
        goal: "Recover the plan lifecycle".into(),
        steps: vec![crate::plan::PlanStep {
            id: "recover".into(),
            title: "Recover after restart".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let mut planning = basic_session("planning-on-restart", "Planning on restart");
    planning.pending_plan = Some(crate::PendingPlan::new(
        "planning-plan".into(),
        0,
        0,
        10,
        1,
        crate::plan::PlanStatus::Planning,
        artifact.clone(),
        Vec::new(),
        false,
    ));
    planning.pending_plan.as_mut().unwrap().pending_feedback =
        Some("Keep the recovered question answer".into());
    database.save_session(&planning).await.unwrap();

    let mut executing = basic_session("executing-on-restart", "Executing on restart");
    let mut executing_plan = crate::PendingPlan::new(
        "executing-plan".into(),
        0,
        0,
        10,
        2,
        crate::plan::PlanStatus::Executing,
        artifact,
        Vec::new(),
        false,
    );
    executing_plan.approved_at = Some(20);
    executing_plan.execution_attempt = 1;
    executing.pending_plan = Some(executing_plan);
    database.save_session(&executing).await.unwrap();

    assert_eq!(database.recover_interrupted_plans().await.unwrap(), (1, 1));

    let planning = database
        .load_session(&planning.id)
        .await
        .unwrap()
        .unwrap()
        .pending_plan
        .unwrap();
    assert_eq!(planning.status, crate::plan::PlanStatus::Stopped);
    assert_eq!(planning.approved_at, None);
    assert_eq!(planning.execution_attempt, 0);
    assert_eq!(
        planning.pending_feedback.as_deref(),
        Some("Keep the recovered question answer")
    );
    assert!(planning.finished_at.is_some());

    let executing = database
        .load_session(&executing.id)
        .await
        .unwrap()
        .unwrap()
        .pending_plan
        .unwrap();
    assert_eq!(executing.status, crate::plan::PlanStatus::Stopped);
    assert_eq!(executing.approved_at, Some(20));
    assert_eq!(executing.execution_attempt, 1);
    assert!(executing.finished_at.is_some());

    assert_eq!(database.recover_interrupted_plans().await.unwrap(), (0, 0));

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn clearing_an_active_plan_persists_a_discarded_terminal_state() {
    let (home, database) = open_temp_database("clear-active-plan").await;
    let mut session = basic_session("clear-active-plan", "Clear active plan");
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan-to-clear".into(),
        status: crate::plan::PlanStatus::Ready,
        created_at: 2,
        updated_at: 3,
        ..Default::default()
    });
    database.save_session(&session).await.unwrap();

    session.pending_plan = None;
    session.updated_at = 4;
    database.save_session(&session).await.unwrap();

    let loaded = database.load_session(&session.id).await.unwrap().unwrap();
    let plan = loaded
        .pending_plan
        .expect("cleared plan history should remain available");
    assert_eq!(plan.id, "plan-to-clear");
    assert_eq!(plan.status, crate::plan::PlanStatus::Discarded);
    assert_eq!(plan.finished_at, Some(4));

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn resetting_session_context_removes_all_plan_history() {
    let (home, database) = open_temp_database("reset-plan-history").await;
    let mut session = basic_session("reset-plan-history", "Reset plan history");
    session.messages.extend([
        chat_message("user", "Plan this change", 2),
        chat_message("assistant", "The plan", 3),
    ]);
    session.pending_plan = Some(crate::PendingPlan {
        id: "plan-to-reset".into(),
        original_user_message_index: 1,
        assistant_plan_message_index: 2,
        status: crate::plan::PlanStatus::Completed,
        created_at: 2,
        updated_at: 3,
        finished_at: Some(3),
        ..Default::default()
    });
    database.save_session(&session).await.unwrap();

    session.messages.truncate(1);
    session.pending_plan = None;
    session.updated_at = 4;
    database.reset_session_context(&session).await.unwrap();

    assert!(
        database
            .load_plan_history(&session.id)
            .await
            .unwrap()
            .is_empty()
    );
    let loaded = database.load_session(&session.id).await.unwrap().unwrap();
    assert_eq!(loaded.messages.len(), 1);
    assert!(loaded.pending_plan.is_none());

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn protected_storage_rejects_wal_checkpoint() {
    let (home, database) = open_temp_database("protected-checkpoint").await;
    database
        .save_session(&basic_session("main", "Main"))
        .await
        .unwrap();
    database.protect("simulated storage failure");

    let error = database
        .checkpoint()
        .await
        .expect_err("protected storage must reject WAL checkpoint writes");
    assert!(error.to_string().contains("simulated storage failure"));
    assert_eq!(database.status().mode, StorageMode::Protected);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn session_message_save_rewrites_only_the_changed_tail() {
    let (home, database) = open_temp_database("message-tail").await;
    let mut session = basic_session("tail", "Tail update");
    session.messages.extend([
        chat_message("user", "first", 2),
        chat_message("assistant", "second", 3),
        chat_message("user", "third", 4),
    ]);
    database.save_session(&session).await.unwrap();
    database
        .call(|connection| {
            connection.execute_batch(
                r#"
                CREATE TEMP TABLE message_audit(kind TEXT NOT NULL, position INTEGER NOT NULL);
                CREATE TEMP TRIGGER audit_message_delete AFTER DELETE ON session_messages
                BEGIN INSERT INTO message_audit(kind, position) VALUES ('delete', OLD.position); END;
                CREATE TEMP TRIGGER audit_message_insert AFTER INSERT ON session_messages
                BEGIN INSERT INTO message_audit(kind, position) VALUES ('insert', NEW.position); END;
                "#,
            )?;
            Ok(())
        })
        .await
        .unwrap();

    session.messages[2].content = Some("second changed".to_string());
    session
        .messages
        .push(chat_message("assistant", "fourth", 5));
    database.save_session(&session).await.unwrap();

    let audit = database
        .read(|connection| {
            let mut statement =
                connection.prepare("SELECT kind, position FROM message_audit ORDER BY rowid")?;
            Ok(statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .unwrap();
    assert_eq!(
        audit,
        vec![
            ("delete".to_string(), 2),
            ("delete".to_string(), 3),
            ("insert".to_string(), 2),
            ("insert".to_string(), 3),
            ("insert".to_string(), 4),
        ]
    );
    let loaded = database.load_session("tail").await.unwrap().unwrap();
    assert_eq!(loaded.messages.len(), 5);
    assert_eq!(
        loaded.messages[2].content.as_deref(),
        Some("second changed")
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn group_round_trip_and_session_delete_clean_membership_atomically() {
    let (home, database) = open_temp_database("group-round-trip").await;
    for session in [
        basic_session("main", "Main"),
        basic_session("worker-a", "Worker A"),
        basic_session("worker-b", "Worker B"),
    ] {
        database.save_session(&session).await.unwrap();
    }
    let mut group = crate::session_group::SessionGroup::new(
        "reviewers",
        "Reviewers",
        vec![
            "main".to_string(),
            "worker-a".to_string(),
            "worker-b".to_string(),
        ],
    );
    group.admins = vec!["worker-b".to_string()];
    group.pending_votes.push(crate::session_group::GroupVote {
        id: "vote-a".to_string(),
        action: "remove_member".to_string(),
        target_session_id: "worker-a".to_string(),
        requester_session_id: "worker-b".to_string(),
        approvals: vec!["worker-b".to_string()],
        threshold: 1,
        created_at: 30,
        updated_at: 31,
    });
    group.messages.push(crate::session_group::GroupMessage {
        id: "message-a".to_string(),
        role: "session".to_string(),
        session_id: Some("worker-b".to_string()),
        content: "review complete".to_string(),
        timestamp: 40,
        turn_id: Some("turn-a".to_string()),
        run_id: Some("run-a".to_string()),
    });
    group.runs.push(crate::session_group::GroupRun {
        id: "run-a".to_string(),
        group_id: "reviewers".to_string(),
        session_id: "worker-b".to_string(),
        status: "completed".to_string(),
        prompt: "review".to_string(),
        result_excerpt: Some("done".to_string()),
        error: None,
        created_at: 35,
        updated_at: 40,
        completed_at: Some(40),
    });
    group.updated_at = 1;
    crate::session_group::normalize_group(&mut group);

    database.save_group(&group).await.unwrap();
    assert_eq!(database.load_group("reviewers").await.unwrap(), Some(group));

    let delete_outcome = database.delete_session("worker-b").await.unwrap();
    assert!(delete_outcome.deleted);
    assert_eq!(delete_outcome.affected_group_ids, vec!["reviewers"]);
    let cleaned = database.load_group("reviewers").await.unwrap().unwrap();
    assert_eq!(cleaned.members, vec!["worker-a"]);
    assert!(cleaned.admins.is_empty());
    assert!(cleaned.pending_votes.is_empty());
    assert!(
        cleaned.updated_at > 1,
        "membership cleanup should update the group timestamp"
    );
    assert_eq!(
        cleaned.messages.len(),
        1,
        "historical messages remain readable"
    );
    assert_eq!(cleaned.runs.len(), 1, "historical runs remain readable");

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn stale_group_save_reports_missing_member_without_protecting_storage() {
    let (home, database) = open_temp_database("stale-group-member").await;
    database
        .save_session(&basic_session("worker-a", "Worker A"))
        .await
        .unwrap();
    let group = crate::session_group::SessionGroup::new(
        "reviewers",
        "Reviewers",
        vec!["worker-a".to_string()],
    );
    database.save_group(&group).await.unwrap();
    assert!(database.delete_session("worker-a").await.unwrap().deleted);

    let error = database
        .save_group(&group)
        .await
        .expect_err("stale member should be a domain conflict");
    assert!(
        error
            .to_string()
            .starts_with(GROUP_MISSING_SESSIONS_ERROR_PREFIX)
    );
    assert_eq!(database.status().mode, StorageMode::Healthy);
    assert!(
        database
            .load_group("reviewers")
            .await
            .unwrap()
            .unwrap()
            .members
            .is_empty()
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn deleting_a_votes_only_approver_removes_the_noncanonical_vote() {
    let (home, database) = open_temp_database("delete-only-vote-approver").await;
    for session in [
        basic_session("worker-a", "Worker A"),
        basic_session("worker-b", "Worker B"),
        basic_session("worker-c", "Worker C"),
    ] {
        database.save_session(&session).await.unwrap();
    }
    let mut group = crate::session_group::SessionGroup::new(
        "reviewers",
        "Reviewers",
        vec![
            "worker-a".to_string(),
            "worker-b".to_string(),
            "worker-c".to_string(),
        ],
    );
    group.admins = vec!["worker-b".to_string(), "worker-c".to_string()];
    group.pending_votes.push(crate::session_group::GroupVote {
        id: "vote-a".to_string(),
        action: "remove_member".to_string(),
        target_session_id: "worker-a".to_string(),
        requester_session_id: "worker-c".to_string(),
        approvals: vec!["worker-b".to_string()],
        threshold: 2,
        created_at: 30,
        updated_at: 31,
    });
    database.save_group(&group).await.unwrap();

    let outcome = database.delete_session("worker-b").await.unwrap();
    assert!(outcome.deleted);
    assert_eq!(outcome.affected_group_ids, vec!["reviewers"]);
    let cleaned = database.load_group("reviewers").await.unwrap().unwrap();
    assert_eq!(
        cleaned.members,
        vec!["worker-a".to_string(), "worker-c".to_string()]
    );
    assert_eq!(cleaned.admins, vec!["worker-c"]);
    assert!(cleaned.pending_votes.is_empty());
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[cfg(windows)]
#[tokio::test]
async fn group_save_canonicalizes_windows_session_id_aliases() {
    let (home, database) = open_temp_database("group-member-case").await;
    database
        .save_session(&basic_session("worker-a", "Worker A"))
        .await
        .unwrap();
    let mut group = crate::session_group::SessionGroup::new(
        "reviewers",
        "Reviewers",
        vec!["WORKER-A".to_string()],
    );
    group.admins = vec!["WORKER-A".to_string()];

    database.save_group(&group).await.unwrap();
    let session = database
        .load_session("WORKER-A")
        .await
        .unwrap()
        .expect("session alias should load");
    assert_eq!(session.id, "worker-a");
    let loaded = database
        .load_group("REVIEWERS")
        .await
        .unwrap()
        .expect("group alias should load");
    assert_eq!(loaded.members, vec!["worker-a"]);
    assert_eq!(loaded.admins, vec!["worker-a"]);
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_migration_rejects_storage_namespace_session_ids_before_moving_files() {
    let home = temp_home("legacy-storage-namespace");
    let sessions_dir = home.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let reserved = basic_session("backups", "Backups");
    std::fs::write(
        sessions_dir.join("backups.json"),
        serde_json::to_vec_pretty(&reserved).unwrap(),
    )
    .unwrap();
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let error = migrate_legacy_json_if_needed(&database)
        .await
        .expect_err("reserved storage namespace must abort migration");
    assert!(error.to_string().contains("backups.json"));
    assert!(sessions_dir.join("backups.json").exists());
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[test]
fn legacy_preflight_rejects_sessions_that_collide_with_sqlite_paths() {
    for session_id in [
        "lingclaw.db",
        "lingclaw.db-journal",
        "lingclaw.db-shm",
        "lingclaw.db-wal",
        "sqlite-migration.json",
    ] {
        let home = temp_home("legacy-storage-preflight");
        let sessions_dir = home.join("sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let session = basic_session(session_id, "Storage collision");
        let source = sessions_dir.join(format!("{session_id}.json"));
        std::fs::write(&source, serde_json::to_vec_pretty(&session).unwrap()).unwrap();
        std::fs::create_dir_all(home.join(session_id).join("workspace")).unwrap();

        let database_path = home.join("lingclaw.db");
        let error = preflight_legacy_storage_path_conflicts(&database_path)
            .expect_err("storage-owned legacy Session must fail before SQLite opens");
        assert!(error.to_string().contains(&source.display().to_string()));
        assert!(error.to_string().contains(session_id));
        assert!(
            !database_path.is_file(),
            "preflight must not create the SQLite database"
        );

        remove_home(&home);
    }
}

#[test]
fn legacy_preflight_accepts_ordinary_sessions_without_creating_sqlite() {
    let home = temp_home("legacy-storage-preflight-ok");
    let sessions_dir = home.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session = basic_session("worker-a", "Worker A");
    std::fs::write(
        sessions_dir.join("worker-a.json"),
        serde_json::to_vec_pretty(&session).unwrap(),
    )
    .unwrap();

    let database_path = home.join("lingclaw.db");
    preflight_legacy_storage_path_conflicts(&database_path).unwrap();
    assert!(!database_path.exists());

    remove_home(&home);
}

#[test]
fn migration_journal_artifacts_are_reserved_session_ids() {
    for id in [
        "sqlite-migration.json",
        "sqlite-migration.json.tmp",
        "sqlite-migration.json.lingclaw-save-backup",
        "sqlite-migration.json.recovery.tmp",
        "sqlite-migration.json.recovery-backup",
    ] {
        assert!(
            crate::session_store::validate_session_id(id).is_err(),
            "{id} must be reserved"
        );
        assert!(
            crate::session_store::session_workspace_root_for_delete(id).is_err(),
            "{id} must never resolve as a deletable Session workspace"
        );
    }
}

#[tokio::test]
async fn legacy_migration_rejects_invalid_live_group_members_before_normalizing() {
    let home = temp_home("legacy-invalid-live-group");
    let sessions_dir = home.join("sessions");
    let groups_dir = home.join("groups");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::create_dir_all(&groups_dir).unwrap();
    let worker = basic_session("worker-a", "Worker A");
    std::fs::write(
        sessions_dir.join("worker-a.json"),
        serde_json::to_vec_pretty(&worker).unwrap(),
    )
    .unwrap();
    let mut group = crate::session_group::SessionGroup::new(
        "legacy-group",
        "Legacy Group",
        vec!["worker-a".to_string()],
    );
    group.members.push("bad/member".to_string());
    let group_path = groups_dir.join("legacy-group.json");
    std::fs::write(&group_path, serde_json::to_vec_pretty(&group).unwrap()).unwrap();
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let error = migrate_legacy_json_if_needed(&database)
        .await
        .expect_err("invalid live membership must abort migration");
    assert!(
        error
            .to_string()
            .contains(&group_path.display().to_string())
    );
    assert!(error.to_string().contains("bad/member"));
    assert!(sessions_dir.exists());
    assert!(groups_dir.exists());
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_migration_rejects_noncanonical_group_ids_before_moving_files() {
    let home = temp_home("legacy-noncanonical-group-id");
    let groups_dir = home.join("groups");
    std::fs::create_dir_all(&groups_dir).unwrap();
    let group =
        crate::session_group::SessionGroup::new(" legacy-group ", "Legacy Group", Vec::new());
    let group_path = groups_dir.join(" legacy-group .json");
    std::fs::write(&group_path, serde_json::to_vec_pretty(&group).unwrap()).unwrap();
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let error = migrate_legacy_json_if_needed(&database)
        .await
        .expect_err("noncanonical group ids must abort migration");
    assert!(
        error
            .to_string()
            .contains("group id ' legacy-group ' is not in canonical form")
    );
    assert!(group_path.exists());
    assert!(groups_dir.exists());
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_migration_rejects_noncanonical_session_file_id_before_moving_files() {
    let home = temp_home("legacy-noncanonical-session-file-id");
    let sessions_dir = home.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session = basic_session(" worker-a ", "Worker A");
    let session_path = sessions_dir.join(" worker-a .json");
    std::fs::write(&session_path, serde_json::to_vec_pretty(&session).unwrap()).unwrap();
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let error = migrate_legacy_json_if_needed(&database)
        .await
        .expect_err("noncanonical Session file ids must abort migration");
    assert!(
        error
            .to_string()
            .contains("session id ' worker-a ' is not in canonical form")
    );
    assert!(session_path.exists());
    assert!(sessions_dir.exists());
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_migration_rejects_noncanonical_session_payload_id_before_moving_files() {
    let home = temp_home("legacy-noncanonical-session-payload-id");
    let sessions_dir = home.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let session = basic_session(" worker-a ", "Worker A");
    let session_path = sessions_dir.join("worker-a.json");
    std::fs::write(&session_path, serde_json::to_vec_pretty(&session).unwrap()).unwrap();
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let error = migrate_legacy_json_if_needed(&database)
        .await
        .expect_err("noncanonical Session payload ids must abort migration");
    assert!(error.to_string().contains("session id inside"));
    assert!(
        error
            .to_string()
            .contains("' worker-a ' is not in canonical form")
    );
    assert!(session_path.exists());
    assert!(sessions_dir.exists());
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_migration_rejects_invalid_persisted_session_before_moving_files() {
    let home = temp_home("legacy-invalid-persisted-session");
    let sessions_dir = home.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let mut session = basic_session("worker-a", "Worker A");
    session.think_level = "turbo".to_string();
    let session_path = sessions_dir.join("worker-a.json");
    std::fs::write(&session_path, serde_json::to_vec_pretty(&session).unwrap()).unwrap();
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let error = migrate_legacy_json_if_needed(&database)
        .await
        .expect_err("an unsupported persisted think level must abort migration");
    assert!(
        error
            .to_string()
            .contains(&session_path.display().to_string())
    );
    assert!(error.to_string().contains("think level 'turbo'"));
    assert!(session_path.exists());
    assert!(sessions_dir.exists());
    assert_eq!(database.entity_counts().await.unwrap(), (0, 0));
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_migration_rejects_invalid_persisted_group_before_moving_files() {
    let home = temp_home("legacy-invalid-persisted-group");
    let sessions_dir = home.join("sessions");
    let groups_dir = home.join("groups");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::create_dir_all(&groups_dir).unwrap();
    let worker = basic_session("worker-a", "Worker A");
    let session_path = sessions_dir.join("worker-a.json");
    std::fs::write(&session_path, serde_json::to_vec_pretty(&worker).unwrap()).unwrap();
    let group = crate::session_group::SessionGroup::new(
        "legacy-group",
        " Legacy Group ",
        vec!["worker-a".to_string()],
    );
    let group_path = groups_dir.join("legacy-group.json");
    std::fs::write(&group_path, serde_json::to_vec_pretty(&group).unwrap()).unwrap();
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let error = migrate_legacy_json_if_needed(&database)
        .await
        .expect_err("a noncanonical persisted group name must abort migration");
    assert!(
        error
            .to_string()
            .contains(&group_path.display().to_string())
    );
    assert!(
        error
            .to_string()
            .contains("Persisted group name is not in canonical form")
    );
    assert!(session_path.exists());
    assert!(group_path.exists());
    assert!(sessions_dir.exists());
    assert!(groups_dir.exists());
    assert_eq!(database.entity_counts().await.unwrap(), (0, 0));
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_migration_rejects_pending_votes_that_normalization_would_discard() {
    let home = temp_home("legacy-invalid-live-vote");
    let sessions_dir = home.join("sessions");
    let groups_dir = home.join("groups");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::create_dir_all(&groups_dir).unwrap();
    for session in [
        basic_session("worker-a", "Worker A"),
        basic_session("worker-b", "Worker B"),
    ] {
        std::fs::write(
            sessions_dir.join(format!("{}.json", session.id)),
            serde_json::to_vec_pretty(&session).unwrap(),
        )
        .unwrap();
    }
    let mut group = crate::session_group::SessionGroup::new(
        "legacy-group",
        "Legacy Group",
        vec!["worker-a".to_string(), "worker-b".to_string()],
    );
    group.admins = vec!["worker-a".to_string()];
    group.pending_votes.push(crate::session_group::GroupVote {
        id: "vote-without-approval".to_string(),
        action: "remove_member".to_string(),
        target_session_id: "worker-b".to_string(),
        requester_session_id: "worker-a".to_string(),
        approvals: Vec::new(),
        threshold: 2,
        created_at: 1,
        updated_at: 1,
    });
    let group_path = groups_dir.join("legacy-group.json");
    std::fs::write(&group_path, serde_json::to_vec_pretty(&group).unwrap()).unwrap();
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let error = migrate_legacy_json_if_needed(&database)
        .await
        .expect_err("a pending vote that normalization would drop must abort migration");
    assert!(
        error
            .to_string()
            .contains("normalization would discard or rewrite")
    );
    assert!(sessions_dir.exists());
    assert!(groups_dir.exists());
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[cfg(windows)]
#[tokio::test]
async fn legacy_migration_canonicalizes_windows_group_member_aliases() {
    let home = temp_home("legacy-member-case");
    let sessions_dir = home.join("sessions");
    let groups_dir = home.join("groups");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::create_dir_all(&groups_dir).unwrap();
    let worker = basic_session("worker-a", "Worker A");
    std::fs::write(
        sessions_dir.join("worker-a.json"),
        serde_json::to_vec_pretty(&worker).unwrap(),
    )
    .unwrap();
    let group = crate::session_group::SessionGroup::new(
        "legacy-group",
        "Legacy Group",
        vec!["WORKER-A".to_string()],
    );
    std::fs::write(
        groups_dir.join("legacy-group.json"),
        serde_json::to_vec_pretty(&group).unwrap(),
    )
    .unwrap();
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    migrate_legacy_json_if_needed(&database)
        .await
        .expect("case-insensitive legacy references should migrate");
    let loaded = database
        .load_group("LEGACY-GROUP")
        .await
        .unwrap()
        .expect("migrated group should load by alias");
    assert_eq!(loaded.members, vec!["worker-a"]);
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_json_migration_preserves_the_groups_session_workspace() {
    let home = temp_home("legacy-groups-session-workspace");
    let sessions_dir = home.join("sessions");
    let groups_workspace = home.join("groups/workspace");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::create_dir_all(&groups_workspace).unwrap();
    let session = basic_session("groups", "Groups Session");
    std::fs::write(
        sessions_dir.join("groups.json"),
        serde_json::to_vec_pretty(&session).unwrap(),
    )
    .unwrap();
    let sentinel = groups_workspace.join("keep.txt");
    std::fs::write(&sentinel, b"workspace-data").unwrap();
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let backup = migrate_legacy_json_if_needed(&database)
        .await
        .unwrap()
        .expect("legacy data should create a backup");

    assert_eq!(std::fs::read(&sentinel).unwrap(), b"workspace-data");
    assert!(
        !backup.join("groups/workspace").exists(),
        "the live Session workspace must not remain stranded in the JSON backup"
    );
    assert!(database.load_session("groups").await.unwrap().is_some());

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_json_migration_is_idempotent_and_keeps_a_permanent_backup() {
    let home = temp_home("legacy-migration");
    let sessions_dir = home.join("sessions");
    let groups_dir = home.join("groups");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::create_dir_all(&groups_dir).unwrap();
    let worker = basic_session("worker-a", "Worker A");
    std::fs::write(
        sessions_dir.join("worker-a.json"),
        serde_json::to_vec_pretty(&worker).unwrap(),
    )
    .unwrap();
    let group = crate::session_group::SessionGroup::new(
        "legacy-group",
        "Legacy Group",
        vec!["worker-a".to_string()],
    );
    std::fs::write(
        groups_dir.join("legacy-group.json"),
        serde_json::to_vec_pretty(&group).unwrap(),
    )
    .unwrap();

    let database = Database::open(home.join("lingclaw.db")).await.unwrap();
    let backup = migrate_legacy_json_if_needed(&database)
        .await
        .unwrap()
        .expect("legacy data should create a backup");
    assert!(!sessions_dir.exists());
    assert!(!groups_dir.exists());
    assert!(backup.join("sessions/worker-a.json").exists());
    assert!(backup.join("groups/legacy-group.json").exists());
    assert!(backup.join("migration-manifest.json").exists());
    assert!(database.load_session("worker-a").await.unwrap().is_some());
    assert!(database.load_group("legacy-group").await.unwrap().is_some());
    assert!(
        database
            .metadata("legacy_json_migration")
            .await
            .unwrap()
            .is_some()
    );
    let stale_journal = home.join("sqlite-migration.json");
    std::fs::write(&stale_journal, b"simulated crash after commit").unwrap();
    assert!(
        migrate_legacy_json_if_needed(&database)
            .await
            .unwrap()
            .is_none()
    );
    assert!(!stale_journal.exists());

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_json_migration_prefers_the_newer_valid_temp_file() {
    let home = temp_home("legacy-temp-recovery");
    let sessions_dir = home.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let primary = basic_session("worker-a", "Primary copy");
    std::fs::write(
        sessions_dir.join("worker-a.json"),
        serde_json::to_vec_pretty(&primary).unwrap(),
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let recovered = basic_session("worker-a", "Recovered temporary copy");
    std::fs::write(
        sessions_dir.join("worker-a.json.tmp"),
        serde_json::to_vec_pretty(&recovered).unwrap(),
    )
    .unwrap();

    let database = Database::open(home.join("lingclaw.db")).await.unwrap();
    let backup = migrate_legacy_json_if_needed(&database)
        .await
        .unwrap()
        .expect("legacy data should create a backup");
    let loaded = database.load_session("worker-a").await.unwrap().unwrap();
    assert_eq!(loaded.name, "Recovered temporary copy");
    assert!(backup.join("sessions/worker-a.json").exists());
    assert!(backup.join("sessions/worker-a.json.tmp").exists());

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_json_migration_uses_the_valid_primary_or_temp_snapshot_independently() {
    let home = temp_home("legacy-corrupt-counterpart");
    let sessions_dir = home.join("sessions");
    let groups_dir = home.join("groups");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::create_dir_all(&groups_dir).unwrap();

    std::fs::write(sessions_dir.join("worker-a.json"), b"{ broken primary").unwrap();
    std::fs::write(
        sessions_dir.join("worker-a.json.tmp"),
        serde_json::to_vec_pretty(&basic_session("worker-a", "Recovered from temp")).unwrap(),
    )
    .unwrap();
    std::fs::write(
        sessions_dir.join("worker-b.json"),
        serde_json::to_vec_pretty(&basic_session("worker-b", "Recovered from primary")).unwrap(),
    )
    .unwrap();
    std::fs::write(sessions_dir.join("worker-b.json.tmp"), b"{ broken temp").unwrap();

    let group_a = crate::session_group::SessionGroup::new(
        "group-a",
        "Group from temp",
        vec!["worker-a".to_string()],
    );
    std::fs::write(groups_dir.join("group-a.json"), b"{ broken primary").unwrap();
    std::fs::write(
        groups_dir.join("group-a.json.tmp"),
        serde_json::to_vec_pretty(&group_a).unwrap(),
    )
    .unwrap();
    let group_b = crate::session_group::SessionGroup::new(
        "group-b",
        "Group from primary",
        vec!["worker-b".to_string()],
    );
    std::fs::write(
        groups_dir.join("group-b.json"),
        serde_json::to_vec_pretty(&group_b).unwrap(),
    )
    .unwrap();
    std::fs::write(groups_dir.join("group-b.json.tmp"), b"{ broken temp").unwrap();

    let database = Database::open(home.join("lingclaw.db")).await.unwrap();
    migrate_legacy_json_if_needed(&database)
        .await
        .expect("one valid snapshot per logical id should be sufficient");

    assert_eq!(
        database
            .load_session("worker-a")
            .await
            .unwrap()
            .unwrap()
            .name,
        "Recovered from temp"
    );
    assert_eq!(
        database
            .load_session("worker-b")
            .await
            .unwrap()
            .unwrap()
            .name,
        "Recovered from primary"
    );
    assert_eq!(
        database.load_group("group-a").await.unwrap().unwrap().name,
        "Group from temp"
    );
    assert_eq!(
        database.load_group("group-b").await.unwrap().unwrap().name,
        "Group from primary"
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_json_migration_preserves_historical_group_references_to_deleted_sessions() {
    let home = temp_home("legacy-historical-group-reference");
    let sessions_dir = home.join("sessions");
    let groups_dir = home.join("groups");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::fs::create_dir_all(&groups_dir).unwrap();
    std::fs::write(
        sessions_dir.join("worker-a.json"),
        serde_json::to_vec_pretty(&basic_session("worker-a", "Worker A")).unwrap(),
    )
    .unwrap();
    let mut group = crate::session_group::SessionGroup::new(
        "legacy-group",
        "Legacy Group",
        vec!["worker-a".to_string()],
    );
    group.messages.push(crate::session_group::GroupMessage {
        id: "message-old".to_string(),
        role: "session".to_string(),
        session_id: Some("deleted-worker".to_string()),
        content: "historical reply".to_string(),
        timestamp: 20,
        turn_id: Some("turn-old".to_string()),
        run_id: Some("run-old".to_string()),
    });
    group.runs.push(crate::session_group::GroupRun {
        id: "run-old".to_string(),
        group_id: group.id.clone(),
        session_id: "deleted-worker".to_string(),
        status: "completed".to_string(),
        prompt: "historical prompt".to_string(),
        result_excerpt: Some("historical result".to_string()),
        error: None,
        created_at: 10,
        updated_at: 20,
        completed_at: Some(20),
    });
    std::fs::write(
        groups_dir.join("legacy-group.json"),
        serde_json::to_vec_pretty(&group).unwrap(),
    )
    .unwrap();

    let database = Database::open(home.join("lingclaw.db")).await.unwrap();
    migrate_legacy_json_if_needed(&database)
        .await
        .expect("historical references must not block migration");
    let loaded = database.load_group("legacy-group").await.unwrap().unwrap();
    assert_eq!(loaded.members, vec!["worker-a"]);
    assert_eq!(
        loaded.messages[0].session_id.as_deref(),
        Some("deleted-worker")
    );
    assert_eq!(loaded.runs[0].session_id, "deleted-worker");
    assert_eq!(loaded.runs[0].status, "completed");

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn core_import_transaction_rolls_back_every_entity_on_failure() {
    let (home, database) = open_temp_database("import-rollback").await;
    database
        .save_session(&basic_session("existing", "Existing"))
        .await
        .unwrap();
    let imported = basic_session("imported", "Imported");
    let mut invalid = basic_session("invalid", "Invalid");
    invalid.created_at = u64::MAX;

    let error = database
        .call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            transaction.execute("DELETE FROM groups", [])?;
            transaction.execute("DELETE FROM sessions", [])?;
            session::save_session_record(&transaction, &imported)?;
            session::save_session_record(&transaction, &invalid)?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .expect_err("invalid imported data must abort the transaction");
    assert!(error.to_string().contains("created_at"));
    assert!(database.load_session("existing").await.unwrap().is_some());
    assert!(database.load_session("imported").await.unwrap().is_none());

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_json_migration_resumes_after_directories_were_moved() {
    let home = temp_home("legacy-resume");
    let backup = home.join("backups/sqlite-migration-resume");
    let backup_sessions = backup.join("sessions");
    std::fs::create_dir_all(&backup_sessions).unwrap();
    let session = basic_session("worker-a", "Recovered after interruption");
    let bytes = serde_json::to_vec_pretty(&session).unwrap();
    std::fs::write(backup_sessions.join("worker-a.json"), &bytes).unwrap();
    let journal = serde_json::json!({
        "version": 1,
        "phase": "moved",
        "backup_dir": backup,
        "had_sessions_dir": true,
        "had_groups_dir": false,
        "manifest": {
            "version": 1,
            "created_at": 1,
            "sessions": 1,
            "groups": 0,
            "files": [{
                "kind": "session",
                "id": "worker-a",
                "source": "sessions/worker-a.json",
                "sha256": format!("{:x}", Sha256::digest(&bytes)),
            }],
        },
    });
    std::fs::write(
        home.join("sqlite-migration.json"),
        serde_json::to_vec_pretty(&journal).unwrap(),
    )
    .unwrap();

    let database = Database::open(home.join("lingclaw.db")).await.unwrap();
    let resumed_backup = migrate_legacy_json_if_needed(&database)
        .await
        .expect("migration should resume")
        .expect("resumed migration should report its backup");
    assert_eq!(resumed_backup, home.join("backups/sqlite-migration-resume"));
    assert!(!home.join("sqlite-migration.json").exists());
    assert_eq!(
        database
            .load_session("worker-a")
            .await
            .unwrap()
            .unwrap()
            .name,
        "Recovered after interruption"
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_json_migration_recovers_when_only_the_journal_temp_file_remains() {
    let home = temp_home("legacy-journal-temp");
    let (backup, journal) = migration_journal_fixture(
        &home,
        "temp-only",
        "moved",
        "worker-a",
        "Recovered from journal temp",
    );
    std::fs::write(
        home.join("sqlite-migration.json.tmp"),
        serde_json::to_vec_pretty(&journal).unwrap(),
    )
    .unwrap();

    let database = Database::open(home.join("lingclaw.db")).await.unwrap();
    let resumed = migrate_legacy_json_if_needed(&database)
        .await
        .expect("the journal temp file should be recoverable")
        .expect("the moved migration should resume");
    assert_eq!(resumed, backup);
    assert_eq!(
        database
            .load_session("worker-a")
            .await
            .unwrap()
            .unwrap()
            .name,
        "Recovered from journal temp"
    );
    for artifact in [
        "sqlite-migration.json",
        "sqlite-migration.json.tmp",
        "sqlite-migration.json.lingclaw-save-backup",
    ] {
        assert!(!home.join(artifact).exists());
    }

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_json_migration_recovers_when_only_the_journal_backup_remains() {
    let home = temp_home("legacy-journal-backup");
    let (backup, journal) = migration_journal_fixture(
        &home,
        "backup-only",
        "moved",
        "worker-a",
        "Recovered from journal backup",
    );
    std::fs::write(
        home.join("sqlite-migration.json.lingclaw-save-backup"),
        serde_json::to_vec_pretty(&journal).unwrap(),
    )
    .unwrap();

    let database = Database::open(home.join("lingclaw.db")).await.unwrap();
    let resumed = migrate_legacy_json_if_needed(&database)
        .await
        .expect("the journal backup should be recoverable")
        .expect("the moved migration should resume");
    assert_eq!(resumed, backup);
    assert_eq!(
        database
            .load_session("worker-a")
            .await
            .unwrap()
            .unwrap()
            .name,
        "Recovered from journal backup"
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn legacy_json_migration_prefers_a_later_valid_journal_phase_across_sidecars() {
    let home = temp_home("legacy-journal-phase");
    let (_prepared_backup, prepared) = migration_journal_fixture(
        &home,
        "prepared-copy",
        "prepared",
        "prepared-session",
        "Prepared copy",
    );
    let (moved_backup, moved) =
        migration_journal_fixture(&home, "moved-copy", "moved", "moved-session", "Moved copy");
    std::fs::write(
        home.join("sqlite-migration.json.lingclaw-save-backup"),
        serde_json::to_vec_pretty(&prepared).unwrap(),
    )
    .unwrap();
    std::fs::write(
        home.join("sqlite-migration.json.tmp"),
        serde_json::to_vec_pretty(&moved).unwrap(),
    )
    .unwrap();

    let database = Database::open(home.join("lingclaw.db")).await.unwrap();
    let resumed = migrate_legacy_json_if_needed(&database)
        .await
        .expect("the later valid phase should be recoverable")
        .expect("the moved migration should resume");
    assert_eq!(resumed, moved_backup);
    assert!(
        database
            .load_session("moved-session")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        database
            .load_session("prepared-session")
            .await
            .unwrap()
            .is_none()
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn corrupt_migration_journals_abort_instead_of_marking_an_empty_database_complete() {
    let home = temp_home("legacy-corrupt-journals");
    std::fs::create_dir_all(&home).unwrap();
    for artifact in [
        "sqlite-migration.json",
        "sqlite-migration.json.tmp",
        "sqlite-migration.json.lingclaw-save-backup",
    ] {
        std::fs::write(home.join(artifact), b"{ not a migration journal").unwrap();
    }
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let error = migrate_legacy_json_if_needed(&database)
        .await
        .expect_err("unrecoverable migration journals must stop startup");
    assert!(error.to_string().contains("Corrupt migration journal"));
    assert!(
        database
            .metadata("legacy_json_migration")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(database.entity_counts().await.unwrap(), (0, 0));

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn stranded_migration_backup_without_a_journal_aborts_instead_of_marking_complete() {
    let home = temp_home("legacy-stranded-backup");
    let (backup, _journal) =
        migration_journal_fixture(&home, "stranded", "moved", "worker-a", "Stranded migration");
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let error = migrate_legacy_json_if_needed(&database)
        .await
        .expect_err("a stranded migration backup must stop startup");
    let message = error.to_string();
    assert!(message.contains("without a recoverable migration journal"));
    assert!(message.contains("sqlite-migration-stranded"));
    assert!(backup.join("sessions/worker-a.json").exists());
    assert!(
        database
            .metadata("legacy_json_migration")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(database.entity_counts().await.unwrap(), (0, 0));

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn corrupt_legacy_json_aborts_before_files_are_moved() {
    let home = temp_home("corrupt-legacy");
    let sessions_dir = home.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let corrupt_path = sessions_dir.join("broken.json");
    let corrupt_tmp_path = sessions_dir.join("broken.json.tmp");
    std::fs::write(&corrupt_path, b"{ definitely not json").unwrap();
    std::fs::write(&corrupt_tmp_path, b"{ also definitely not json").unwrap();
    let database = Database::open(home.join("lingclaw.db")).await.unwrap();

    let error = migrate_legacy_json_if_needed(&database)
        .await
        .expect_err("corrupt JSON must stop migration");
    assert!(
        error
            .to_string()
            .contains(corrupt_path.to_string_lossy().as_ref())
    );
    assert!(
        error
            .to_string()
            .contains(corrupt_tmp_path.to_string_lossy().as_ref())
    );
    assert!(corrupt_path.exists());
    assert!(corrupt_tmp_path.exists());
    assert_eq!(database.entity_counts().await.unwrap(), (0, 0));

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn online_backup_is_consistent_and_refuses_to_overwrite() {
    let (home, database) = open_temp_database("backup").await;
    database
        .save_session(&basic_session("main", "Main"))
        .await
        .unwrap();
    let destination = home.join("backups/snapshot.db");

    admin::create_backup(database.path(), &destination).expect("backup should succeed");
    let backup = rusqlite::Connection::open(&destination).unwrap();
    let application_id = backup
        .query_row("PRAGMA application_id", [], |row| row.get::<_, i64>(0))
        .unwrap();
    let quick_check = backup
        .query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0))
        .unwrap();
    let session_name = backup
        .query_row("SELECT name FROM sessions WHERE id='main'", [], |row| {
            row.get::<_, String>(0)
        })
        .optional()
        .unwrap();
    assert_eq!(application_id, schema::APPLICATION_ID);
    assert_eq!(quick_check, "ok");
    assert_eq!(session_name.as_deref(), Some("Main"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(
        admin::create_backup(database.path(), &destination)
            .expect_err("existing destination must be rejected")
            .to_string()
            .contains("already exists")
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn online_backup_rejects_foreign_key_violations() {
    let (home, database) = open_temp_database("backup-foreign-key").await;
    {
        let connection = rusqlite::Connection::open(database.path()).unwrap();
        connection
            .pragma_update(None, "foreign_keys", false)
            .unwrap();
        connection
            .execute(
                "INSERT INTO session_messages(session_id, position, role, fingerprint) \
                 VALUES ('missing-session', 0, 'user', 'orphan')",
                [],
            )
            .unwrap();
    }
    let destination = home.join("backups/orphaned.db");

    let error = admin::create_backup(database.path(), &destination)
        .expect_err("backup must reject orphaned rows");
    assert!(
        error.to_string().contains("foreign key check failed"),
        "unexpected error: {error}"
    );
    assert!(
        !destination.exists(),
        "a rejected source must not create a backup"
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn lightweight_usage_query_filters_the_day_and_loaded_sessions_without_reading_messages() {
    let (home, database) = open_temp_database("usage-lightweight").await;
    let mut loaded = basic_session("loaded", "Loaded");
    loaded.token_usage_day = "2026-07-20".to_string();
    loaded.daily_input_tokens = 10;
    loaded.daily_output_tokens = 20;
    let mut persisted = basic_session("persisted", "Persisted");
    persisted.token_usage_day = "2026-07-20".to_string();
    persisted.daily_input_tokens = 30;
    persisted.daily_output_tokens = 40;
    persisted
        .messages
        .push(chat_message("user", &"x".repeat(1_000_000), 2));
    let mut old = basic_session("old", "Old");
    old.token_usage_day = "2026-07-19".to_string();
    old.daily_input_tokens = 50;
    old.daily_output_tokens = 60;
    for session in [&loaded, &persisted, &old] {
        database.save_session(session).await.unwrap();
    }
    database
        .call(|connection| {
            connection.execute(
                "UPDATE session_messages SET images_json='{ invalid json' \
                 WHERE session_id='persisted'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let excluding_loaded = database
        .current_usage_excluding("2026-07-20", &HashSet::from(["loaded".to_string()]))
        .await
        .expect("usage should not deserialize the corrupt or long message");
    assert_eq!(excluding_loaded, (30, 40));
    assert_eq!(
        database
            .current_usage_excluding("2026-07-20", &HashSet::new())
            .await
            .unwrap(),
        (40, 60)
    );
    let persisted_snapshot = database
        .load_usage_snapshot("persisted", "2026-07-20")
        .await
        .expect("lightweight Session usage should not parse messages")
        .expect("persisted Session should exist");
    assert_eq!(persisted_snapshot.daily_input, 30);
    assert_eq!(persisted_snapshot.daily_output, 40);
    let old_snapshot = database
        .load_usage_snapshot("old", "2026-07-20")
        .await
        .expect("stale usage should load")
        .expect("old Session should exist");
    assert_eq!(old_snapshot.daily_input, 0);
    assert_eq!(old_snapshot.daily_output, 0);
    assert!(
        old_snapshot
            .usage_history
            .iter()
            .any(|day| day.date == "2026-07-19" && day.input == 50 && day.output == 60)
    );
    assert_eq!(database.status().mode, StorageMode::Healthy);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn group_roster_gate_serializes_member_addition_with_session_deletion() {
    let (home, database) = open_temp_database("group-roster-race").await;
    database
        .save_session(&basic_session("worker-a", "Worker A"))
        .await
        .unwrap();
    let group = crate::session_group::SessionGroup::new("reviewers", "Reviewers", Vec::new());
    database.save_group(&group).await.unwrap();

    let start = Arc::new(tokio::sync::Barrier::new(3));
    let observed_rosters = Arc::new(tokio::sync::Mutex::new(Vec::<Vec<String>>::new()));
    let add_task = {
        let database = database.clone();
        let start = Arc::clone(&start);
        let observed_rosters = Arc::clone(&observed_rosters);
        tokio::spawn(async move {
            start.wait().await;
            let roster_gate = crate::session_group::group_roster_gate();
            let _roster_guard = roster_gate.lock().await;
            let group_gate = crate::session_group::group_persist_gate("reviewers");
            let _group_guard = group_gate.lock().await;
            let mut group = database.load_group("reviewers").await.unwrap().unwrap();
            group.members = vec!["worker-a".to_string()];
            let result = database.save_group(&group).await;
            if result.is_ok() {
                observed_rosters.lock().await.push(
                    database
                        .load_group("reviewers")
                        .await
                        .unwrap()
                        .unwrap()
                        .members,
                );
            }
            result
        })
    };
    let delete_task = {
        let database = database.clone();
        let start = Arc::clone(&start);
        let observed_rosters = Arc::clone(&observed_rosters);
        tokio::spawn(async move {
            start.wait().await;
            let roster_gate = crate::session_group::group_roster_gate();
            let _roster_guard = roster_gate.lock().await;
            let group_gate = crate::session_group::group_persist_gate("reviewers");
            let _group_guard = group_gate.lock().await;
            let outcome = database.delete_session("worker-a").await.unwrap();
            observed_rosters.lock().await.push(
                database
                    .load_group("reviewers")
                    .await
                    .unwrap()
                    .unwrap()
                    .members,
            );
            outcome
        })
    };
    start.wait().await;
    let add_result = add_task.await.unwrap();
    let delete_outcome = delete_task.await.unwrap();

    assert!(delete_outcome.deleted);
    if let Err(error) = add_result {
        assert!(
            error
                .to_string()
                .starts_with(GROUP_MISSING_SESSIONS_ERROR_PREFIX)
        );
    }
    assert!(
        database
            .load_group("reviewers")
            .await
            .unwrap()
            .unwrap()
            .members
            .is_empty()
    );
    assert!(observed_rosters.lock().await.last().unwrap().is_empty());

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn noncanonical_group_vote_enters_sticky_protected_mode_during_rebuild() {
    let (home, database) = open_temp_database("protected-group-vote").await;
    for session in [
        basic_session("worker-a", "Worker A"),
        basic_session("worker-b", "Worker B"),
    ] {
        database.save_session(&session).await.unwrap();
    }
    let mut group = crate::session_group::SessionGroup::new(
        "reviewers",
        "Reviewers",
        vec!["worker-a".to_string(), "worker-b".to_string()],
    );
    group.admins = vec!["worker-a".to_string()];
    group.pending_votes.push(crate::session_group::GroupVote {
        id: "vote-a".to_string(),
        action: "remove_member".to_string(),
        target_session_id: "worker-b".to_string(),
        requester_session_id: "worker-a".to_string(),
        approvals: vec!["worker-a".to_string()],
        threshold: 1,
        created_at: 10,
        updated_at: 10,
    });
    database.save_group(&group).await.unwrap();
    database
        .call(|connection| {
            connection.execute(
                "UPDATE group_votes SET action='unsupported' \
                 WHERE group_id='reviewers' AND vote_id='vote-a'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let error = database
        .load_group("reviewers")
        .await
        .expect_err("normalization must not silently discard persisted votes");
    assert!(error.to_string().contains("not in canonical form"));
    assert_eq!(database.status().mode, StorageMode::Protected);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn invalid_group_run_status_enters_sticky_protected_mode_during_rebuild() {
    let (home, database) = open_temp_database("protected-group-run-status").await;
    database
        .save_session(&basic_session("worker-a", "Worker A"))
        .await
        .unwrap();
    let mut group = crate::session_group::SessionGroup::new(
        "reviewers",
        "Reviewers",
        vec!["worker-a".to_string()],
    );
    group.runs.push(crate::session_group::GroupRun {
        id: "run-a".to_string(),
        group_id: group.id.clone(),
        session_id: "worker-a".to_string(),
        status: "completed".to_string(),
        prompt: "Review".to_string(),
        result_excerpt: Some("Done".to_string()),
        error: None,
        created_at: 10,
        updated_at: 11,
        completed_at: Some(11),
    });
    database.save_group(&group).await.unwrap();
    database
        .call(|connection| {
            connection.execute(
                "UPDATE group_runs SET status='mystery' \
                 WHERE group_id='reviewers' AND run_id='run-a'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let error = database
        .load_group("reviewers")
        .await
        .expect_err("unknown run status must fail the read");
    assert!(error.to_string().contains("Invalid group run status"));
    assert_eq!(database.status().mode, StorageMode::Protected);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn invalid_message_json_enters_sticky_protected_mode_during_rebuild() {
    let (home, database) = open_temp_database("protected-message-json").await;
    database
        .save_session(&basic_session("main", "Main"))
        .await
        .unwrap();
    database
        .call(|connection| {
            connection.execute(
                "UPDATE session_messages SET images_json='{ invalid json' WHERE session_id='main'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let error = match database.load_session("main").await {
        Err(error) => error,
        Ok(_) => panic!("invalid persisted JSON should fail the read"),
    };
    let reason = error.to_string();
    assert!(!reason.is_empty());
    assert_eq!(database.status().mode, StorageMode::Protected);
    assert_eq!(database.status().reason.as_deref(), Some(reason.as_str()));
    assert!(
        database
            .save_session(&basic_session("blocked", "Blocked"))
            .await
            .expect_err("sticky protection must reject later writes")
            .to_string()
            .contains(&reason)
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn invalid_plan_artifact_enters_sticky_protected_mode_during_rebuild() {
    let (home, database) = open_temp_database("protected-plan-artifact").await;
    let session = populated_session();
    let session_id = session.id.clone();
    database.save_session(&session).await.unwrap();
    let corrupt_session_id = session_id.clone();
    database
        .call(move |connection| {
            connection.execute(
                r#"UPDATE session_plan_revisions
                   SET artifact_json='{"schema_version":1,"title":"","goal":"Invalid","steps":[{"id":"inspect","title":"Inspect"}]}'
                   WHERE session_id=?1"#,
                [&corrupt_session_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let error = match database.load_session(&session_id).await {
        Err(error) => error,
        Ok(_) => panic!("semantically invalid plan data must fail the read"),
    };
    assert!(error.to_string().contains("Invalid persisted plan"));
    assert!(error.to_string().contains("title must contain"));
    assert_eq!(database.status().mode, StorageMode::Protected);
    assert!(
        database
            .save_session(&basic_session("blocked-plan-write", "Blocked"))
            .await
            .expect_err("sticky protection must reject later writes")
            .to_string()
            .contains("Invalid persisted plan")
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn invalid_session_top_level_fields_enter_sticky_protected_mode() {
    let cases = [
        (
            "version",
            "UPDATE sessions SET version=999 WHERE id='main'",
            "Invalid persisted session version",
        ),
        (
            "think-level",
            "UPDATE sessions SET think_level='turbo' WHERE id='main'",
            "Invalid persisted session think level",
        ),
        (
            "show-react",
            "UPDATE sessions SET show_react=2 WHERE id='main'",
            "Invalid persisted session show_react flag",
        ),
        (
            "show-tools",
            "UPDATE sessions SET show_tools=-1 WHERE id='main'",
            "Invalid persisted session show_tools flag",
        ),
        (
            "show-reasoning",
            "UPDATE sessions SET show_reasoning=3 WHERE id='main'",
            "Invalid persisted session show_reasoning flag",
        ),
    ];

    for (label, sql, expected) in cases {
        let (home, database) = open_temp_database(label).await;
        database
            .save_session(&basic_session("main", "Main"))
            .await
            .unwrap();
        database
            .call(move |connection| {
                connection.execute(sql, [])?;
                Ok(())
            })
            .await
            .unwrap();

        let error = match database.load_session("main").await {
            Err(error) => error,
            Ok(_) => panic!("invalid Session metadata must fail the read"),
        };
        assert!(
            error.to_string().contains(expected),
            "unexpected error for {label}: {error}"
        );
        assert_eq!(database.status().mode, StorageMode::Protected);

        drop(database);
        remove_home(&home);
    }
}

#[tokio::test]
async fn invalid_usage_label_references_enter_sticky_protected_mode_during_rebuild() {
    let cases = [
        (
            "usage-current-bucket",
            "current",
            "wrong-day",
            "Invalid usage label bucket",
        ),
        (
            "usage-total-bucket",
            "total",
            "unexpected",
            "Invalid usage label bucket",
        ),
        (
            "usage-missing-history",
            "history",
            "1999-01-01",
            "Usage label references missing history day",
        ),
    ];

    for (case, scope, bucket, expected) in cases {
        let (home, database) = open_temp_database(case).await;
        database
            .save_session(&basic_session("main", "Main"))
            .await
            .unwrap();
        let scope = scope.to_string();
        let bucket = bucket.to_string();
        database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO session_usage_labels \
                     (session_id, scope, bucket, label, input, output) \
                     VALUES ('main', ?1, ?2, 'provider:test', 1, 2)",
                    rusqlite::params![scope, bucket],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let error = match database.load_session("main").await {
            Err(error) => error,
            Ok(_) => panic!("invalid Usage labels must fail the Session rebuild"),
        };
        assert!(
            error.to_string().contains(expected),
            "unexpected error for {case}: {error}"
        );
        assert_eq!(database.status().mode, StorageMode::Protected);

        drop(database);
        remove_home(&home);
    }
}

#[tokio::test]
async fn noncanonical_session_identity_enters_protected_mode_during_summary_read() {
    let (home, database) = open_temp_database("protected-session-name").await;
    database
        .save_session(&basic_session("main", "Main"))
        .await
        .unwrap();
    database
        .call(|connection| {
            connection.execute("UPDATE sessions SET name=' Main ' WHERE id='main'", [])?;
            Ok(())
        })
        .await
        .unwrap();

    let error = match database.list_session_summaries().await {
        Err(error) => error,
        Ok(_) => panic!("a noncanonical Session name must fail the summary read"),
    };
    assert!(
        error
            .to_string()
            .contains("Persisted session name is not in canonical form")
    );
    assert_eq!(database.status().mode, StorageMode::Protected);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn lightweight_session_queries_reject_noncanonical_persisted_identity() {
    let cases = [
        ("protected-session-id-list", false),
        ("protected-session-name-map", true),
    ];
    for (case, corrupt_name) in cases {
        let (home, database) = open_temp_database(case).await;
        database
            .call(move |connection| {
                connection.execute(
                    r#"INSERT INTO sessions(
                        id, name, created_at, updated_at, tool_calls_count, model_override,
                        think_level, show_react, show_tools, show_reasoning,
                        visible_message_count, version
                    ) VALUES (?1, ?2, 1, 1, 0, NULL, 'auto', 1, 1, 1, 0, ?3)"#,
                    rusqlite::params![
                        if corrupt_name {
                            "valid-id"
                        } else {
                            "invalid/id"
                        },
                        if corrupt_name { " Invalid " } else { "Invalid" },
                        i64::from(crate::SESSION_VERSION),
                    ],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let error = if corrupt_name {
            database
                .session_name_map()
                .await
                .expect_err("the name map must reject a noncanonical Session name")
        } else {
            database
                .list_session_ids()
                .await
                .expect_err("the id list must reject an invalid Session id")
        };
        assert!(
            error.to_string().contains(if corrupt_name {
                "Persisted session name is not in canonical form"
            } else {
                "Invalid persisted session id"
            }),
            "unexpected error for {case}: {error}"
        );
        assert_eq!(database.status().mode, StorageMode::Protected);

        drop(database);
        remove_home(&home);
    }
}

#[tokio::test]
async fn lightweight_group_id_query_rejects_invalid_persisted_identity() {
    let (home, database) = open_temp_database("protected-group-id-list").await;
    database
        .call(|connection| {
            connection.execute(
                "INSERT INTO groups(id, name, created_at, updated_at, version) \
                 VALUES ('invalid/id', 'Invalid', 1, 1, ?1)",
                [i64::from(crate::session_group::GROUP_VERSION)],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let error = database
        .list_group_ids()
        .await
        .expect_err("the Group id list must reject an invalid persisted id");
    assert!(
        error.to_string().contains("Invalid persisted group id"),
        "unexpected error: {error}"
    );
    assert_eq!(database.status().mode, StorageMode::Protected);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn noncontiguous_message_positions_block_tail_rewrite_without_data_loss() {
    let (home, database) = open_temp_database("protected-message-position").await;
    let mut session = basic_session("main", "Main");
    session.messages.push(chat_message("user", "first", 2));
    session
        .messages
        .push(chat_message("assistant", "second", 3));
    database.save_session(&session).await.unwrap();
    database
        .call(|connection| {
            connection.execute(
                "UPDATE session_messages SET position=3 \
                 WHERE session_id='main' AND position=2",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let error = database
        .save_session(&session)
        .await
        .expect_err("a message position gap must block the tail rewrite");
    assert!(
        error
            .to_string()
            .contains("Invalid session message position 3; expected 2")
    );
    assert_eq!(database.status().mode, StorageMode::Protected);
    let persisted = database
        .read(|connection| {
            let mut statement = connection.prepare(
                "SELECT position, content FROM session_messages \
                 WHERE session_id='main' ORDER BY position",
            )?;
            Ok(statement
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
        .await
        .expect("protected storage should remain readable");
    assert_eq!(
        persisted,
        vec![
            (0, Some("system prompt".to_string())),
            (1, Some("first".to_string())),
            (3, Some("second".to_string())),
        ]
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn noncontiguous_message_positions_enter_protected_mode_during_rebuild() {
    let (home, database) = open_temp_database("protected-message-position-read").await;
    let mut session = basic_session("main", "Main");
    session.messages.push(chat_message("user", "first", 2));
    session
        .messages
        .push(chat_message("assistant", "second", 3));
    database.save_session(&session).await.unwrap();
    database
        .call(|connection| {
            connection.execute(
                "UPDATE session_messages SET position=3 \
                 WHERE session_id='main' AND position=2",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let error = match database.load_session("main").await {
        Err(error) => error,
        Ok(_) => panic!("a message position gap must fail the rebuild"),
    };
    assert!(
        error
            .to_string()
            .contains("Invalid session message position 3; expected 2")
    );
    assert_eq!(database.status().mode, StorageMode::Protected);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn mismatched_message_fingerprint_enters_protected_mode() {
    let (home, database) = open_temp_database("protected-message-fingerprint").await;
    database
        .save_session(&basic_session("main", "Main"))
        .await
        .unwrap();
    database
        .call(|connection| {
            connection.execute(
                "UPDATE session_messages SET fingerprint='tampered' \
                 WHERE session_id='main' AND position=0",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let error = match database.load_session("main").await {
        Err(error) => error,
        Ok(_) => panic!("a message fingerprint mismatch must fail the read"),
    };
    assert!(
        error
            .to_string()
            .contains("Session message fingerprint mismatch at position 0")
    );
    assert_eq!(database.status().mode, StorageMode::Protected);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn invalid_todo_status_enters_sticky_protected_mode_during_rebuild() {
    let (home, database) = open_temp_database("protected-todo-status").await;
    let mut session = basic_session("main", "Main");
    session.todos.items.push(crate::todos::TodoItem {
        id: "todo-a".to_string(),
        content: "Persist me".to_string(),
        status: crate::todos::TodoStatus::Pending,
    });
    database.save_session(&session).await.unwrap();
    database
        .call(|connection| {
            connection.execute(
                "UPDATE session_todos SET status='invalid' WHERE session_id='main'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let error = match database.load_session("main").await {
        Err(error) => error,
        Ok(_) => panic!("invalid todo status should fail the read"),
    };
    assert!(error.to_string().contains("Invalid todo status"));
    assert_eq!(database.status().mode, StorageMode::Protected);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn noncanonical_todo_state_enters_sticky_protected_mode_during_rebuild() {
    let (home, database) = open_temp_database("protected-todo-canonical-state").await;
    let mut session = basic_session("main", "Main");
    session.todos.items.extend([
        crate::todos::TodoItem {
            id: "todo-a".to_string(),
            content: "First task".to_string(),
            status: crate::todos::TodoStatus::Pending,
        },
        crate::todos::TodoItem {
            id: "todo-b".to_string(),
            content: "Second task".to_string(),
            status: crate::todos::TodoStatus::Pending,
        },
    ]);
    database.save_session(&session).await.unwrap();
    database
        .call(|connection| {
            connection.execute(
                "UPDATE session_todos SET status='in_progress' WHERE session_id='main'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let error = match database.load_session("main").await {
        Err(error) => error,
        Ok(_) => panic!("noncanonical Todo state must fail the read"),
    };
    assert!(
        error
            .to_string()
            .contains("Persisted Todo state is not in canonical form")
    );
    assert_eq!(database.status().mode, StorageMode::Protected);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn invalid_group_numbers_enter_sticky_protected_mode_during_summary_read() {
    let (home, database) = open_temp_database("protected-group-number").await;
    database
        .save_group(&crate::session_group::SessionGroup::new(
            "reviewers",
            "Reviewers",
            Vec::new(),
        ))
        .await
        .unwrap();
    database
        .call(|connection| {
            connection.execute("UPDATE groups SET created_at=-1 WHERE id='reviewers'", [])?;
            Ok(())
        })
        .await
        .unwrap();

    let error = database
        .list_group_summaries()
        .await
        .expect_err("invalid group summary range should fail the read");
    assert!(
        error
            .to_string()
            .contains("Invalid negative group created_at")
    );
    assert_eq!(database.status().mode, StorageMode::Protected);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn invalid_group_identity_enters_sticky_protected_mode_during_summary_read() {
    for (case, sql, expected_error) in [
        (
            "id",
            "UPDATE groups SET id='bad/group' WHERE id='reviewers'",
            "Invalid persisted group id",
        ),
        (
            "canonical-id",
            "UPDATE groups SET id=' reviewers ' WHERE id='reviewers'",
            "Persisted group id is not in canonical form",
        ),
        (
            "name",
            "UPDATE groups SET name=' Reviewers ' WHERE id='reviewers'",
            "Persisted group name is not in canonical form",
        ),
        (
            "version",
            "UPDATE groups SET version=999 WHERE id='reviewers'",
            "Invalid persisted group version 999",
        ),
    ] {
        let (home, database) =
            open_temp_database(&format!("protected-group-identity-{case}")).await;
        database
            .save_group(&crate::session_group::SessionGroup::new(
                "reviewers",
                "Reviewers",
                Vec::new(),
            ))
            .await
            .unwrap();
        database
            .call(move |connection| {
                connection.execute(sql, [])?;
                Ok(())
            })
            .await
            .unwrap();

        let error = database
            .list_group_summaries()
            .await
            .expect_err("invalid group identity should fail the summary read");
        assert!(error.to_string().contains(expected_error));
        assert_eq!(database.status().mode, StorageMode::Protected);

        drop(database);
        remove_home(&home);
    }
}

#[tokio::test]
async fn invalid_session_numbers_enter_sticky_protected_mode_during_summary_read() {
    let (home, database) = open_temp_database("protected-session-summary-number").await;
    database
        .save_session(&basic_session("main", "Main"))
        .await
        .unwrap();
    database
        .call(|connection| {
            connection.execute("UPDATE sessions SET updated_at=-1 WHERE id='main'", [])?;
            Ok(())
        })
        .await
        .unwrap();

    let error = match database.list_session_summaries().await {
        Err(error) => error,
        Ok(_) => panic!("invalid Session summary range should fail the read"),
    };
    assert!(
        error
            .to_string()
            .contains("Invalid negative session updated_at")
    );
    assert_eq!(database.status().mode, StorageMode::Protected);

    drop(database);
    remove_home(&home);
}

#[tokio::test]
async fn a_storage_failure_enters_sticky_protected_mode_but_keeps_reads_available() {
    let (home, database) = open_temp_database("protected").await;
    database
        .save_session(&basic_session("main", "Main"))
        .await
        .unwrap();

    let failure = database
        .call(|_| Err::<(), _>(StorageError::new("simulated disk failure")))
        .await
        .expect_err("failure should be returned");
    assert!(failure.to_string().contains("simulated disk failure"));
    assert_eq!(database.status().mode, StorageMode::Protected);
    assert!(
        database
            .status()
            .reason
            .unwrap()
            .contains("simulated disk failure")
    );
    assert!(
        database
            .save_session(&basic_session("blocked", "Blocked"))
            .await
            .expect_err("writes must remain blocked")
            .to_string()
            .contains("simulated disk failure")
    );
    assert_eq!(
        database
            .load_session("main")
            .await
            .expect("reads remain available")
            .unwrap()
            .name,
        "Main"
    );

    drop(database);
    remove_home(&home);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn queued_write_is_rejected_after_a_prior_read_protects_storage() {
    let (home, database) = open_temp_database("protected-queued-write").await;
    let (read_started_tx, read_started_rx) = std::sync::mpsc::sync_channel(0);
    let (release_read_tx, release_read_rx) = std::sync::mpsc::sync_channel(0);
    let failing_database = database.clone();
    let failing_read = tokio::spawn(async move {
        failing_database
            .read(move |_| {
                read_started_tx.send(()).unwrap();
                release_read_rx.recv().unwrap();
                Err::<(), _>(StorageError::new("simulated read failure"))
            })
            .await
    });
    tokio::task::spawn_blocking(move || {
        read_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("failing read should start")
    })
    .await
    .unwrap();

    let write_attempted = Arc::new(AtomicBool::new(false));
    let write_executed = Arc::new(AtomicBool::new(false));
    let queued_database = database.clone();
    let queued_write_attempted = write_attempted.clone();
    let queued_write_executed = write_executed.clone();
    let queued_write = tokio::spawn(async move {
        queued_write_attempted.store(true, Ordering::SeqCst);
        queued_database
            .call(move |connection| {
                queued_write_executed.store(true, Ordering::SeqCst);
                connection.execute(
                    "INSERT INTO storage_metadata(key, value) VALUES ('queued-write', 'ran')",
                    [],
                )?;
                Ok(())
            })
            .await
    });
    while !write_attempted.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    release_read_tx.send(()).unwrap();

    let read_error = failing_read
        .await
        .unwrap()
        .expect_err("the first read should fail");
    assert!(read_error.to_string().contains("simulated read failure"));
    let write_error = queued_write
        .await
        .unwrap()
        .expect_err("the queued write must observe protected mode");
    assert!(write_error.to_string().contains("simulated read failure"));
    assert!(!write_executed.load(Ordering::SeqCst));
    assert_eq!(database.status().mode, StorageMode::Protected);
    assert_eq!(database.metadata("queued-write").await.unwrap(), None);

    drop(database);
    remove_home(&home);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_failed_read_still_protects_before_a_queued_write_runs() {
    let (home, database) = open_temp_database("protected-cancelled-read").await;
    let (read_started_tx, read_started_rx) = std::sync::mpsc::sync_channel(0);
    let (release_read_tx, release_read_rx) = std::sync::mpsc::sync_channel(0);
    let failing_database = database.clone();
    let failing_read = tokio::spawn(async move {
        failing_database
            .read(move |_| {
                read_started_tx.send(()).unwrap();
                release_read_rx.recv().unwrap();
                Err::<(), _>(StorageError::new("cancelled read failure"))
            })
            .await
    });
    tokio::task::spawn_blocking(move || {
        read_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("failing read should start")
    })
    .await
    .unwrap();
    failing_read.abort();
    assert!(failing_read.await.unwrap_err().is_cancelled());

    let write_attempted = Arc::new(AtomicBool::new(false));
    let write_executed = Arc::new(AtomicBool::new(false));
    let queued_database = database.clone();
    let queued_write_attempted = write_attempted.clone();
    let queued_write_executed = write_executed.clone();
    let queued_write = tokio::spawn(async move {
        queued_write_attempted.store(true, Ordering::SeqCst);
        queued_database
            .call(move |connection| {
                queued_write_executed.store(true, Ordering::SeqCst);
                connection.execute(
                    "INSERT INTO storage_metadata(key, value) VALUES ('cancelled-queued-write', 'ran')",
                    [],
                )?;
                Ok(())
            })
            .await
    });
    while !write_attempted.load(Ordering::SeqCst) {
        tokio::task::yield_now().await;
    }
    tokio::time::sleep(Duration::from_millis(20)).await;
    release_read_tx.send(()).unwrap();

    let write_error = queued_write
        .await
        .unwrap()
        .expect_err("the queued write must observe the worker-side protected state");
    assert!(write_error.to_string().contains("cancelled read failure"));
    assert!(!write_executed.load(Ordering::SeqCst));
    assert_eq!(database.status().mode, StorageMode::Protected);
    assert_eq!(
        database.metadata("cancelled-queued-write").await.unwrap(),
        None
    );

    drop(database);
    remove_home(&home);
}
