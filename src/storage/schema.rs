pub(crate) const APPLICATION_ID: i64 = 0x4C_43_4C_57;
pub(crate) const SCHEMA_VERSION: i64 = 6;

pub(crate) const INITIAL_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS storage_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY COLLATE BINARY,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    tool_calls_count INTEGER NOT NULL,
    model_override TEXT,
    think_level TEXT NOT NULL,
    show_react INTEGER NOT NULL,
    show_tools INTEGER NOT NULL,
    show_reasoning INTEGER NOT NULL,
    visible_message_count INTEGER NOT NULL,
    version INTEGER NOT NULL,
    workspace_kind TEXT NOT NULL DEFAULT 'managed',
    working_directory TEXT NOT NULL DEFAULT '',
    working_directory_key TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS idx_sessions_updated_at ON sessions(updated_at DESC, id);
CREATE INDEX IF NOT EXISTS idx_sessions_working_directory
    ON sessions(working_directory_key, updated_at DESC, id);

CREATE TABLE IF NOT EXISTS session_messages (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    role TEXT NOT NULL,
    content TEXT,
    images_json TEXT,
    thinking TEXT,
    thinking_blocks_json TEXT,
    tool_calls_json TEXT,
    tool_call_id TEXT,
    timestamp INTEGER,
    fingerprint TEXT NOT NULL,
    PRIMARY KEY (session_id, position)
);
CREATE INDEX IF NOT EXISTS idx_session_messages_tool_call
    ON session_messages(session_id, tool_call_id) WHERE tool_call_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS session_skills (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    skill_id TEXT NOT NULL,
    PRIMARY KEY (session_id, skill_id)
);

CREATE TABLE IF NOT EXISTS session_failed_tool_results (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    tool_call_id TEXT NOT NULL,
    PRIMARY KEY (session_id, tool_call_id)
);

CREATE TABLE IF NOT EXISTS session_subagent_snapshots (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    storage_key TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    PRIMARY KEY (session_id, storage_key)
);

CREATE TABLE IF NOT EXISTS session_todo_state (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    last_updated_by TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS session_todos (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    todo_id TEXT NOT NULL,
    content TEXT NOT NULL,
    status TEXT NOT NULL,
    PRIMARY KEY (session_id, position),
    UNIQUE (session_id, todo_id)
);

CREATE TABLE IF NOT EXISTS session_plans (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    plan_id TEXT NOT NULL,
    original_user_message_index INTEGER NOT NULL,
    current_revision INTEGER NOT NULL,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    approved_at INTEGER,
    finished_at INTEGER,
    execution_attempt INTEGER NOT NULL,
    evidence_truncated INTEGER NOT NULL,
    stale_override_json TEXT,
    pending_feedback TEXT,
    initial_submission_pending INTEGER NOT NULL DEFAULT 0,
    stale_override_confirmed_at INTEGER,
    PRIMARY KEY (session_id, plan_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_session_plans_one_active
    ON session_plans(session_id)
    WHERE status IN ('planning', 'needs_input', 'ready', 'executing');
CREATE INDEX IF NOT EXISTS idx_session_plans_latest
    ON session_plans(session_id, updated_at DESC, plan_id DESC);

CREATE TABLE IF NOT EXISTS session_plan_revisions (
    session_id TEXT NOT NULL,
    plan_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    assistant_plan_message_index INTEGER NOT NULL,
    artifact_json TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, plan_id, revision),
    FOREIGN KEY (session_id, plan_id)
        REFERENCES session_plans(session_id, plan_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS session_plan_progress (
    session_id TEXT NOT NULL,
    plan_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    step_id TEXT NOT NULL,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    note TEXT NOT NULL,
    deviation_reason TEXT,
    PRIMARY KEY (session_id, plan_id, position),
    UNIQUE (session_id, plan_id, step_id),
    FOREIGN KEY (session_id, plan_id)
        REFERENCES session_plans(session_id, plan_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS session_usage (
    session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
    total_input INTEGER NOT NULL,
    total_output INTEGER NOT NULL,
    current_input INTEGER NOT NULL,
    current_output INTEGER NOT NULL,
    input_source TEXT NOT NULL,
    output_source TEXT NOT NULL,
    current_day TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS session_usage_days (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    date TEXT NOT NULL,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    PRIMARY KEY (session_id, date)
);

CREATE TABLE IF NOT EXISTS session_usage_labels (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    scope TEXT NOT NULL,
    bucket TEXT NOT NULL,
    label TEXT NOT NULL,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    PRIMARY KEY (session_id, scope, bucket, label)
);

CREATE TABLE IF NOT EXISTS groups (
    id TEXT PRIMARY KEY COLLATE BINARY,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    version INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_groups_updated_at ON groups(updated_at DESC, id);

CREATE TABLE IF NOT EXISTS group_members (
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    is_admin INTEGER NOT NULL,
    PRIMARY KEY (group_id, session_id),
    UNIQUE (group_id, position)
);

CREATE TABLE IF NOT EXISTS group_votes (
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    vote_id TEXT NOT NULL,
    action TEXT NOT NULL,
    target_session_id TEXT NOT NULL,
    requester_session_id TEXT NOT NULL,
    threshold INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (group_id, vote_id)
);

CREATE TABLE IF NOT EXISTS group_vote_approvals (
    group_id TEXT NOT NULL,
    vote_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    PRIMARY KEY (group_id, vote_id, session_id),
    FOREIGN KEY (group_id, vote_id) REFERENCES group_votes(group_id, vote_id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS group_messages (
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    message_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    role TEXT NOT NULL,
    session_id TEXT,
    content TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    turn_id TEXT,
    run_id TEXT,
    PRIMARY KEY (group_id, message_id),
    UNIQUE (group_id, position)
);

CREATE TABLE IF NOT EXISTS group_runs (
    group_id TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    session_id TEXT NOT NULL,
    status TEXT NOT NULL,
    prompt TEXT NOT NULL,
    result_excerpt TEXT,
    error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER,
    PRIMARY KEY (group_id, run_id),
    UNIQUE (group_id, position)
);
"#;

pub(crate) const PLAN_LIFECYCLE_SCHEMA: &str = r#"
CREATE TABLE session_plans (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    plan_id TEXT NOT NULL,
    original_user_message_index INTEGER NOT NULL,
    current_revision INTEGER NOT NULL,
    status TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    approved_at INTEGER,
    finished_at INTEGER,
    execution_attempt INTEGER NOT NULL,
    evidence_truncated INTEGER NOT NULL,
    stale_override_json TEXT,
    PRIMARY KEY (session_id, plan_id)
);
CREATE UNIQUE INDEX idx_session_plans_one_active
    ON session_plans(session_id)
    WHERE status IN ('planning', 'needs_input', 'ready', 'executing');
CREATE INDEX idx_session_plans_latest
    ON session_plans(session_id, updated_at DESC, plan_id DESC);
CREATE TABLE session_plan_revisions (
    session_id TEXT NOT NULL,
    plan_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    assistant_plan_message_index INTEGER NOT NULL,
    artifact_json TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (session_id, plan_id, revision),
    FOREIGN KEY (session_id, plan_id)
        REFERENCES session_plans(session_id, plan_id) ON DELETE CASCADE
);
CREATE TABLE session_plan_progress (
    session_id TEXT NOT NULL,
    plan_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    step_id TEXT NOT NULL,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    note TEXT NOT NULL,
    deviation_reason TEXT,
    PRIMARY KEY (session_id, plan_id, position),
    UNIQUE (session_id, plan_id, step_id),
    FOREIGN KEY (session_id, plan_id)
        REFERENCES session_plans(session_id, plan_id) ON DELETE CASCADE
);
"#;

pub(crate) const PLAN_FEEDBACK_SCHEMA: &str = r#"
ALTER TABLE session_plans ADD COLUMN pending_feedback TEXT;
"#;

pub(crate) const PLAN_INITIAL_SUBMISSION_SCHEMA: &str = r#"
ALTER TABLE session_plans ADD COLUMN initial_submission_pending INTEGER NOT NULL DEFAULT 0;
"#;

pub(crate) const PLAN_STALE_OVERRIDE_AUDIT_SCHEMA: &str = r#"
ALTER TABLE session_plans ADD COLUMN stale_override_confirmed_at INTEGER;
"#;

pub(crate) const SESSION_WORKSPACE_SCHEMA: &str = r#"
ALTER TABLE sessions ADD COLUMN workspace_kind TEXT NOT NULL DEFAULT 'managed';
ALTER TABLE sessions ADD COLUMN working_directory TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN working_directory_key TEXT NOT NULL DEFAULT '';
CREATE INDEX idx_sessions_working_directory
    ON sessions(working_directory_key, updated_at DESC, id);
"#;
