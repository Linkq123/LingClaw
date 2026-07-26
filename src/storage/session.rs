use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{Database, StorageError};
use crate::{
    ChatMessage, DailyUsageSnapshot, PendingPlan, Session, SubagentHistorySnapshot,
    session_store::{SessionSummary, sanitized_non_system_message_count},
    todos::{TodoItem, TodoSnapshot, TodoStatus, TodoUpdatedBy},
};

#[derive(Clone)]
struct StoredMessage {
    position: i64,
    role: String,
    content: Option<String>,
    images_json: Option<String>,
    thinking: Option<String>,
    thinking_blocks_json: Option<String>,
    tool_calls_json: Option<String>,
    tool_call_id: Option<String>,
    timestamp: Option<i64>,
    fingerprint: String,
}

struct StoredSession {
    session: Session,
    messages: Vec<StoredMessage>,
    skills: Vec<String>,
    failed_tool_results: Vec<String>,
    subagent_snapshots: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SessionDeleteOutcome {
    pub(crate) deleted: bool,
    pub(crate) affected_group_ids: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SessionUsageSnapshot {
    pub(crate) daily_input: u64,
    pub(crate) daily_output: u64,
    pub(crate) total_input: u64,
    pub(crate) total_output: u64,
    pub(crate) input_source: String,
    pub(crate) output_source: String,
    pub(crate) usage_history: Vec<DailyUsageSnapshot>,
    pub(crate) daily_labels: HashMap<String, [u64; 2]>,
    pub(crate) total_labels: HashMap<String, [u64; 2]>,
}

#[derive(Default)]
struct LoadedSession {
    id: String,
    name: String,
    created_at: i64,
    updated_at: i64,
    tool_calls_count: i64,
    model_override: Option<String>,
    think_level: String,
    show_react: i64,
    show_tools: i64,
    show_reasoning: i64,
    version: i64,
    messages: Vec<StoredMessage>,
    skills: Vec<String>,
    failed_tool_results: Vec<String>,
    subagent_snapshots: Vec<(String, String)>,
    todo_state: Option<(i64, String, i64)>,
    todos: Vec<(String, String, String)>,
    pending_plan: Option<(String, i64, i64, i64)>,
    usage: Option<(i64, i64, i64, i64, String, String, String)>,
    usage_days: Vec<(String, i64, i64)>,
    usage_labels: Vec<(String, String, String, i64, i64)>,
}

fn json_string<T: Serialize>(value: &T) -> Result<String, StorageError> {
    serde_json::to_string(value).map_err(|error| StorageError::new(error.to_string()))
}

fn optional_json<T: Serialize>(value: Option<&T>) -> Result<Option<String>, StorageError> {
    value.map(json_string).transpose()
}

fn to_i64(value: u64, field: &str) -> Result<i64, StorageError> {
    i64::try_from(value)
        .map_err(|_| StorageError::new(format!("{field} exceeds SQLite INTEGER range")))
}

fn to_usize(value: i64, field: &str) -> Result<usize, StorageError> {
    usize::try_from(value)
        .map_err(|_| StorageError::new(format!("Invalid negative or oversized {field}")))
}

fn to_u64(value: i64, field: &str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::new(format!("Invalid negative {field}")))
}

fn validate_persisted_session_id(id: &str) -> Result<(), StorageError> {
    let canonical_id = crate::session_store::validate_session_id(id)
        .map_err(|error| StorageError::new(format!("Invalid persisted session id: {error}")))?;
    if canonical_id != id {
        return Err(StorageError::new(
            "Persisted session id is not in canonical form",
        ));
    }
    Ok(())
}

fn validate_persisted_session_identity(id: &str, name: &str) -> Result<(), StorageError> {
    validate_persisted_session_id(id)?;
    let canonical_name = crate::validate_session_display_name(name)
        .map_err(|error| StorageError::new(format!("Invalid persisted session name: {error}")))?;
    if canonical_name != name {
        return Err(StorageError::new(
            "Persisted session name is not in canonical form",
        ));
    }
    Ok(())
}

fn parse_session_flag(value: i64, field: &str) -> Result<bool, StorageError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(StorageError::new(format!(
            "Invalid persisted session {field} flag '{value}'"
        ))),
    }
}

fn message_fingerprint(message: &ChatMessage) -> Result<String, StorageError> {
    let payload =
        serde_json::to_vec(message).map_err(|error| StorageError::new(error.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(payload)))
}

fn prepare_session(session: &Session) -> Result<StoredSession, StorageError> {
    let session = crate::session_store::session_for_storage(session).map_err(StorageError::new)?;
    let messages = session
        .messages
        .iter()
        .enumerate()
        .map(|(position, message)| {
            Ok(StoredMessage {
                position: i64::try_from(position)
                    .map_err(|_| StorageError::new("Too many session messages"))?,
                role: message.role.clone(),
                content: message.content.clone(),
                images_json: optional_json(message.images.as_ref())?,
                thinking: message.thinking.clone(),
                thinking_blocks_json: optional_json(message.anthropic_thinking_blocks.as_ref())?,
                tool_calls_json: optional_json(message.tool_calls.as_ref())?,
                tool_call_id: message.tool_call_id.clone(),
                timestamp: message
                    .timestamp
                    .map(|value| to_i64(value, "message timestamp"))
                    .transpose()?,
                fingerprint: message_fingerprint(message)?,
            })
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    let mut skills = session
        .enabled_system_skills
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    skills.sort();
    let mut failed_tool_results = session
        .failed_tool_results
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    failed_tool_results.sort();
    let mut subagent_snapshots = session
        .subagent_snapshots
        .iter()
        .map(|(key, snapshot)| Ok((key.clone(), json_string(snapshot)?)))
        .collect::<Result<Vec<_>, StorageError>>()?;
    subagent_snapshots.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(StoredSession {
        session,
        messages,
        skills,
        failed_tool_results,
        subagent_snapshots,
    })
}

fn todo_updated_by_label(value: TodoUpdatedBy) -> &'static str {
    match value {
        TodoUpdatedBy::User => "user",
        TodoUpdatedBy::Assistant => "assistant",
    }
}

fn todo_status_label(value: TodoStatus) -> &'static str {
    match value {
        TodoStatus::Pending => "pending",
        TodoStatus::InProgress => "in_progress",
        TodoStatus::Completed => "completed",
    }
}

fn parse_todo_updated_by(value: &str) -> Result<TodoUpdatedBy, StorageError> {
    match value {
        "user" => Ok(TodoUpdatedBy::User),
        "assistant" => Ok(TodoUpdatedBy::Assistant),
        _ => Err(StorageError::new(format!(
            "Invalid todo update origin '{value}'"
        ))),
    }
}

fn parse_todo_status(value: &str) -> Result<TodoStatus, StorageError> {
    match value {
        "pending" => Ok(TodoStatus::Pending),
        "in_progress" => Ok(TodoStatus::InProgress),
        "completed" => Ok(TodoStatus::Completed),
        _ => Err(StorageError::new(format!("Invalid todo status '{value}'"))),
    }
}

fn save_prepared_session(
    connection: &rusqlite::Connection,
    stored: &StoredSession,
) -> Result<(), StorageError> {
    let session = &stored.session;
    connection.execute(
        r#"INSERT INTO sessions(
            id, name, created_at, updated_at, tool_calls_count, model_override,
            think_level, show_react, show_tools, show_reasoning,
            visible_message_count, version
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT(id) DO UPDATE SET
            name=excluded.name,
            created_at=excluded.created_at,
            updated_at=excluded.updated_at,
            tool_calls_count=excluded.tool_calls_count,
            model_override=excluded.model_override,
            think_level=excluded.think_level,
            show_react=excluded.show_react,
            show_tools=excluded.show_tools,
            show_reasoning=excluded.show_reasoning,
            visible_message_count=excluded.visible_message_count,
            version=excluded.version"#,
        params![
            session.id,
            session.name,
            to_i64(session.created_at, "session created_at")?,
            to_i64(session.updated_at, "session updated_at")?,
            i64::try_from(session.tool_calls_count).map_err(|_| rusqlite::Error::InvalidQuery)?,
            session.model_override,
            session.think_level,
            i64::from(session.show_react),
            i64::from(session.show_tools),
            i64::from(session.show_reasoning),
            i64::try_from(sanitized_non_system_message_count(session))
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
            i64::from(session.version),
        ],
    )?;

    let persisted_messages = {
        let mut statement = connection.prepare(
            "SELECT position, fingerprint FROM session_messages \
             WHERE session_id=?1 ORDER BY position",
        )?;
        statement
            .query_map([&session.id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    for (expected_position, (position, _)) in persisted_messages.iter().enumerate() {
        let expected_position = i64::try_from(expected_position)
            .map_err(|_| StorageError::new("Too many persisted session messages"))?;
        if *position != expected_position {
            return Err(StorageError::new(format!(
                "Invalid session message position {position}; expected {expected_position}"
            )));
        }
    }
    let common_prefix = persisted_messages
        .iter()
        .zip(stored.messages.iter())
        .take_while(|((_, fingerprint), message)| fingerprint == &message.fingerprint)
        .count();
    connection.execute(
        "DELETE FROM session_messages WHERE session_id=?1 AND position>=?2",
        params![session.id, i64::try_from(common_prefix).unwrap_or(i64::MAX)],
    )?;
    {
        let mut statement = connection.prepare(
            r#"INSERT INTO session_messages(
                session_id, position, role, content, images_json, thinking,
                thinking_blocks_json, tool_calls_json, tool_call_id, timestamp, fingerprint
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
        )?;
        for message in stored.messages.iter().skip(common_prefix) {
            statement.execute(params![
                session.id,
                message.position,
                message.role,
                message.content,
                message.images_json,
                message.thinking,
                message.thinking_blocks_json,
                message.tool_calls_json,
                message.tool_call_id,
                message.timestamp,
                message.fingerprint,
            ])?;
        }
    }

    connection.execute(
        "DELETE FROM session_skills WHERE session_id=?1",
        [&session.id],
    )?;
    for skill in &stored.skills {
        connection.execute(
            "INSERT INTO session_skills(session_id, skill_id) VALUES (?1, ?2)",
            params![session.id, skill],
        )?;
    }
    connection.execute(
        "DELETE FROM session_failed_tool_results WHERE session_id=?1",
        [&session.id],
    )?;
    for tool_call_id in &stored.failed_tool_results {
        connection.execute(
            "INSERT INTO session_failed_tool_results(session_id, tool_call_id) VALUES (?1, ?2)",
            params![session.id, tool_call_id],
        )?;
    }
    connection.execute(
        "DELETE FROM session_subagent_snapshots WHERE session_id=?1",
        [&session.id],
    )?;
    for (storage_key, snapshot_json) in &stored.subagent_snapshots {
        connection.execute(
            "INSERT INTO session_subagent_snapshots(session_id, storage_key, snapshot_json) VALUES (?1, ?2, ?3)",
            params![session.id, storage_key, snapshot_json],
        )?;
    }

    connection.execute(
        r#"INSERT INTO session_todo_state(session_id, revision, last_updated_by, updated_at)
           VALUES (?1, ?2, ?3, ?4)
           ON CONFLICT(session_id) DO UPDATE SET
             revision=excluded.revision,
             last_updated_by=excluded.last_updated_by,
             updated_at=excluded.updated_at"#,
        params![
            session.id,
            to_i64(session.todos.revision, "todo revision")?,
            todo_updated_by_label(session.todos.last_updated_by),
            to_i64(session.todos.updated_at, "todo updated_at")?,
        ],
    )?;
    connection.execute(
        "DELETE FROM session_todos WHERE session_id=?1",
        [&session.id],
    )?;
    for (position, todo) in session.todos.items.iter().enumerate() {
        connection.execute(
            "INSERT INTO session_todos(session_id, position, todo_id, content, status) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                session.id,
                i64::try_from(position).map_err(|_| rusqlite::Error::InvalidQuery)?,
                todo.id,
                todo.content,
                todo_status_label(todo.status),
            ],
        )?;
    }

    connection.execute(
        "DELETE FROM session_pending_plans WHERE session_id=?1",
        [&session.id],
    )?;
    if let Some(plan) = &session.pending_plan {
        connection.execute(
            r#"INSERT INTO session_pending_plans(
                session_id, plan_id, original_user_message_index,
                assistant_plan_message_index, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5)"#,
            params![
                session.id,
                plan.id,
                i64::try_from(plan.original_user_message_index)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                i64::try_from(plan.assistant_plan_message_index)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                to_i64(plan.created_at, "pending plan created_at")?,
            ],
        )?;
    }

    connection.execute(
        r#"INSERT INTO session_usage(
            session_id, total_input, total_output, current_input, current_output,
            input_source, output_source, current_day
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(session_id) DO UPDATE SET
            total_input=excluded.total_input,
            total_output=excluded.total_output,
            current_input=excluded.current_input,
            current_output=excluded.current_output,
            input_source=excluded.input_source,
            output_source=excluded.output_source,
            current_day=excluded.current_day"#,
        params![
            session.id,
            to_i64(session.input_tokens, "total input tokens")?,
            to_i64(session.output_tokens, "total output tokens")?,
            to_i64(session.daily_input_tokens, "daily input tokens")?,
            to_i64(session.daily_output_tokens, "daily output tokens")?,
            session.input_token_source,
            session.output_token_source,
            session.token_usage_day,
        ],
    )?;
    connection.execute(
        "DELETE FROM session_usage_days WHERE session_id=?1",
        [&session.id],
    )?;
    for day in &session.usage_history {
        connection.execute(
            "INSERT INTO session_usage_days(session_id, date, input, output) VALUES (?1, ?2, ?3, ?4)",
            params![
                session.id,
                day.date,
                to_i64(day.input, "usage day input")?,
                to_i64(day.output, "usage day output")?,
            ],
        )?;
    }
    connection.execute(
        "DELETE FROM session_usage_labels WHERE session_id=?1",
        [&session.id],
    )?;
    for (label, values) in &session.daily_provider_usage {
        connection.execute(
            "INSERT INTO session_usage_labels(session_id, scope, bucket, label, input, output) VALUES (?1, 'current', ?2, ?3, ?4, ?5)",
            params![session.id, session.token_usage_day, label, to_i64(values[0], "usage label input")?, to_i64(values[1], "usage label output")?],
        )?;
    }
    for day in &session.usage_history {
        for (label, values) in &day.providers {
            connection.execute(
                "INSERT INTO session_usage_labels(session_id, scope, bucket, label, input, output) VALUES (?1, 'history', ?2, ?3, ?4, ?5)",
                params![session.id, day.date, label, to_i64(values[0], "usage history label input")?, to_i64(values[1], "usage history label output")?],
            )?;
        }
    }
    for (label, values) in &session.total_label_usage {
        connection.execute(
            "INSERT INTO session_usage_labels(session_id, scope, bucket, label, input, output) VALUES (?1, 'total', '', ?2, ?3, ?4)",
            params![session.id, label, to_i64(values[0], "total usage label input")?, to_i64(values[1], "total usage label output")?],
        )?;
    }
    Ok(())
}

pub(super) fn save_session_record(
    connection: &rusqlite::Connection,
    session: &Session,
) -> Result<(), StorageError> {
    let stored = prepare_session(session)?;
    save_prepared_session(connection, &stored)
}

pub(super) fn canonical_session_id_record(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<Option<String>, StorageError> {
    let mut canonical = connection
        .query_row("SELECT id FROM sessions WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    if canonical.is_none() && cfg!(windows) {
        canonical = connection
            .query_row(
                "SELECT id FROM sessions WHERE id=?1 COLLATE NOCASE ORDER BY id LIMIT 1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
    }
    if let Some(canonical) = canonical.as_deref() {
        validate_persisted_session_id(canonical)?;
    }
    Ok(canonical)
}

impl Database {
    pub(crate) async fn save_session(&self, session: &Session) -> Result<(), StorageError> {
        let session = session.clone();
        self.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            save_session_record(&transaction, &session)?;
            transaction.commit()?;
            Ok(())
        })
        .await
    }
    #[cfg(test)]
    pub(crate) async fn load_session(&self, id: &str) -> Result<Option<Session>, StorageError> {
        let id = id.to_string();
        self.read(move |connection| {
            let Some(id) = canonical_session_id_record(connection, &id)? else {
                return Ok(None);
            };
            load_session_record(connection, &id)?
                .map(rebuild_session)
                .transpose()
        })
        .await
    }

    #[cfg(not(test))]
    pub(crate) fn load_session_blocking(&self, id: &str) -> Result<Option<Session>, StorageError> {
        let id = id.to_string();
        self.blocking_read(move |connection| {
            load_session_record(connection, &id)?
                .map(rebuild_session)
                .transpose()
        })
    }

    #[cfg(not(test))]
    pub(crate) fn canonical_session_id_blocking(
        &self,
        id: &str,
    ) -> Result<Option<String>, StorageError> {
        let id = id.to_string();
        self.blocking_read(move |connection| canonical_session_id_record(connection, &id))
    }

    #[cfg(not(test))]
    pub(crate) fn list_session_summaries_blocking(
        &self,
    ) -> Result<Vec<SessionSummary>, StorageError> {
        self.blocking_read(query_session_summaries)
    }

    #[cfg(test)]
    pub(crate) async fn list_session_summaries(&self) -> Result<Vec<SessionSummary>, StorageError> {
        self.read(query_session_summaries).await
    }

    #[cfg(test)]
    pub(crate) async fn list_session_ids(&self) -> Result<HashSet<String>, StorageError> {
        self.read(query_session_ids).await
    }

    #[cfg(test)]
    pub(crate) async fn session_name_map(&self) -> Result<HashMap<String, String>, StorageError> {
        self.read(query_session_name_map).await
    }

    #[cfg(not(test))]
    pub(crate) fn list_session_ids_blocking(&self) -> Result<HashSet<String>, StorageError> {
        self.blocking_read(query_session_ids)
    }

    #[cfg(not(test))]
    pub(crate) fn session_name_map_blocking(
        &self,
    ) -> Result<HashMap<String, String>, StorageError> {
        self.blocking_read(query_session_name_map)
    }

    pub(crate) async fn delete_session(
        &self,
        id: &str,
    ) -> Result<SessionDeleteOutcome, StorageError> {
        let id = id.to_string();
        self.call(move |connection| {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let affected_group_ids = query_group_ids_referencing_session(&transaction, &id)?;
            transaction.execute(
                r#"DELETE FROM group_votes
                   WHERE EXISTS (
                       SELECT 1 FROM group_vote_approvals approval
                       WHERE approval.group_id=group_votes.group_id
                         AND approval.vote_id=group_votes.vote_id
                         AND approval.session_id=?1
                   )
                     AND NOT EXISTS (
                       SELECT 1 FROM group_vote_approvals remaining
                       WHERE remaining.group_id=group_votes.group_id
                         AND remaining.vote_id=group_votes.vote_id
                         AND remaining.session_id<>?1
                   )"#,
                [&id],
            )?;
            transaction.execute(
                "DELETE FROM group_vote_approvals WHERE session_id=?1",
                [&id],
            )?;
            transaction.execute(
                "DELETE FROM group_votes WHERE target_session_id=?1 OR requester_session_id=?1",
                [&id],
            )?;
            transaction.execute("DELETE FROM group_members WHERE session_id=?1", [&id])?;
            let group_updated_at = to_i64(crate::now_epoch(), "group updated_at")?;
            for group_id in &affected_group_ids {
                transaction.execute(
                    "UPDATE groups SET updated_at=?1 WHERE id=?2",
                    params![group_updated_at, group_id],
                )?;
            }
            let deleted = transaction.execute("DELETE FROM sessions WHERE id=?1", [&id])? > 0;
            transaction.commit()?;
            Ok(SessionDeleteOutcome {
                deleted,
                affected_group_ids,
            })
        })
        .await
    }

    pub(crate) async fn current_usage_excluding(
        &self,
        today: &str,
        excluded_session_ids: &HashSet<String>,
    ) -> Result<(u64, u64), StorageError> {
        let today = today.to_string();
        let excluded_session_ids = excluded_session_ids.clone();
        self.read(move |connection| {
            let mut statement = connection.prepare(
                "SELECT session_id, current_input, current_output FROM session_usage \
                 WHERE current_day=?1",
            )?;
            let rows = statement.query_map([&today], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            let mut input = 0_u64;
            let mut output = 0_u64;
            for row in rows {
                let (session_id, row_input, row_output) = row?;
                if excluded_session_ids.contains(&session_id) {
                    continue;
                }
                input = input.saturating_add(to_u64(row_input, "daily input tokens")?);
                output = output.saturating_add(to_u64(row_output, "daily output tokens")?);
            }
            Ok((input, output))
        })
        .await
    }

    pub(crate) async fn load_usage_snapshot(
        &self,
        id: &str,
        today: &str,
    ) -> Result<Option<SessionUsageSnapshot>, StorageError> {
        let id = id.to_string();
        let today = today.to_string();
        self.read(move |connection| {
            let Some(id) = canonical_session_id_record(connection, &id)? else {
                return Ok(None);
            };
            let usage = connection
                .query_row(
                    "SELECT total_input, total_output, current_input, current_output, \
                            input_source, output_source, current_day \
                     FROM session_usage WHERE session_id=?1",
                    [&id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, String>(5)?,
                            row.get::<_, String>(6)?,
                        ))
                    },
                )
                .optional()?
                .unwrap_or_else(|| {
                    (
                        0,
                        0,
                        0,
                        0,
                        crate::default_token_usage_source(),
                        crate::default_token_usage_source(),
                        String::new(),
                    )
                });

            let mut usage_history = {
                let mut statement = connection.prepare(
                    "SELECT date, input, output FROM session_usage_days \
                     WHERE session_id=?1 ORDER BY date",
                )?;
                statement
                    .query_map([&id], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    })?
                    .map(|row| {
                        let (date, input, output) = row?;
                        Ok(DailyUsageSnapshot {
                            date,
                            input: to_u64(input, "usage day input")?,
                            output: to_u64(output, "usage day output")?,
                            providers: HashMap::new(),
                        })
                    })
                    .collect::<Result<Vec<_>, StorageError>>()?
            };

            let mut current_labels = HashMap::new();
            let mut total_labels = HashMap::new();
            {
                let mut statement = connection.prepare(
                    "SELECT scope, bucket, label, input, output FROM session_usage_labels \
                     WHERE session_id=?1 ORDER BY scope, bucket, label",
                )?;
                let rows = statement.query_map([&id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                })?;
                for row in rows {
                    let (scope, bucket, label, input, output) = row?;
                    let values = [
                        to_u64(input, "usage label input")?,
                        to_u64(output, "usage label output")?,
                    ];
                    match scope.as_str() {
                        "current" if bucket == usage.6 => {
                            current_labels.insert(label, values);
                        }
                        "history" => {
                            let Some(day) = usage_history.iter_mut().find(|day| day.date == bucket)
                            else {
                                return Err(StorageError::new(format!(
                                    "Usage label references missing history day '{bucket}'"
                                )));
                            };
                            day.providers.insert(label, values);
                        }
                        "total" if bucket.is_empty() => {
                            total_labels.insert(label, values);
                        }
                        "current" | "total" => {
                            return Err(StorageError::new(format!(
                                "Invalid usage label bucket '{bucket}' for scope '{scope}'"
                            )));
                        }
                        _ => {
                            return Err(StorageError::new(format!(
                                "Invalid usage label scope '{scope}'"
                            )));
                        }
                    }
                }
            }

            let mut daily_input = to_u64(usage.2, "daily input tokens")?;
            let mut daily_output = to_u64(usage.3, "daily output tokens")?;
            if usage.6 != today {
                if (daily_input > 0 || daily_output > 0) && !usage.6.is_empty() {
                    usage_history.push(DailyUsageSnapshot {
                        date: usage.6,
                        input: daily_input,
                        output: daily_output,
                        providers: current_labels,
                    });
                    if usage_history.len() > crate::USAGE_HISTORY_CAP {
                        let excess = usage_history.len() - crate::USAGE_HISTORY_CAP;
                        usage_history.drain(..excess);
                    }
                }
                daily_input = 0;
                daily_output = 0;
                current_labels = HashMap::new();
            }

            Ok(Some(SessionUsageSnapshot {
                daily_input,
                daily_output,
                total_input: to_u64(usage.0, "total input tokens")?,
                total_output: to_u64(usage.1, "total output tokens")?,
                input_source: usage.4,
                output_source: usage.5,
                usage_history,
                daily_labels: current_labels,
                total_labels,
            }))
        })
        .await
    }
}

fn query_group_ids_referencing_session(
    connection: &rusqlite::Connection,
    session_id: &str,
) -> Result<Vec<String>, StorageError> {
    let mut statement = connection.prepare(
        r#"SELECT group_id FROM group_members WHERE session_id=?1
           UNION
           SELECT group_id FROM group_votes
             WHERE target_session_id=?1 OR requester_session_id=?1
           UNION
           SELECT group_id FROM group_vote_approvals WHERE session_id=?1
           ORDER BY group_id"#,
    )?;
    Ok(statement
        .query_map([session_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn query_session_summaries(
    connection: &mut rusqlite::Connection,
) -> Result<Vec<SessionSummary>, StorageError> {
    let mut statement = connection.prepare(
        r#"SELECT id, name, model_override, visible_message_count,
                  tool_calls_count, created_at, updated_at
           FROM sessions ORDER BY
             CASE WHEN lower(id)='main' THEN 0 ELSE 1 END,
             updated_at DESC, id"#,
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(id, name, model_override, messages, tool_calls, created_at, updated_at)| {
                validate_persisted_session_identity(&id, &name)?;
                Ok(SessionSummary {
                    id,
                    name,
                    model_override,
                    messages: to_usize(messages, "visible message count")?,
                    tool_calls: to_usize(tool_calls, "tool call count")?,
                    created_at: to_u64(created_at, "session created_at")?,
                    updated_at: to_u64(updated_at, "session updated_at")?,
                    corrupt: false,
                })
            },
        )
        .collect()
}

fn query_session_ids(
    connection: &mut rusqlite::Connection,
) -> Result<HashSet<String>, StorageError> {
    let mut statement = connection.prepare("SELECT id FROM sessions ORDER BY id")?;
    let ids = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<HashSet<String>, _>>()?;
    for id in &ids {
        validate_persisted_session_id(id)?;
    }
    Ok(ids)
}

fn query_session_name_map(
    connection: &mut rusqlite::Connection,
) -> Result<HashMap<String, String>, StorageError> {
    let mut statement = connection.prepare("SELECT id, name FROM sessions")?;
    let names = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<HashMap<String, String>, _>>()?;
    for (id, name) in &names {
        validate_persisted_session_identity(id, name)?;
    }
    Ok(names)
}

fn load_session_record(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<Option<LoadedSession>, StorageError> {
    let Some(mut loaded) = connection
        .query_row(
            r#"SELECT id, name, created_at, updated_at, tool_calls_count,
                      model_override, think_level, show_react, show_tools,
                      show_reasoning, version
               FROM sessions WHERE id=?1"#,
            [id],
            |row| {
                Ok(LoadedSession {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                    updated_at: row.get(3)?,
                    tool_calls_count: row.get(4)?,
                    model_override: row.get(5)?,
                    think_level: row.get(6)?,
                    show_react: row.get(7)?,
                    show_tools: row.get(8)?,
                    show_reasoning: row.get(9)?,
                    version: row.get(10)?,
                    ..LoadedSession::default()
                })
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    {
        let mut statement = connection.prepare(
            r#"SELECT position, role, content, images_json, thinking,
                      thinking_blocks_json, tool_calls_json, tool_call_id,
                      timestamp, fingerprint
               FROM session_messages WHERE session_id=?1 ORDER BY position"#,
        )?;
        loaded.messages = statement
            .query_map([id], |row| {
                Ok(StoredMessage {
                    position: row.get(0)?,
                    role: row.get(1)?,
                    content: row.get(2)?,
                    images_json: row.get(3)?,
                    thinking: row.get(4)?,
                    thinking_blocks_json: row.get(5)?,
                    tool_calls_json: row.get(6)?,
                    tool_call_id: row.get(7)?,
                    timestamp: row.get(8)?,
                    fingerprint: row.get(9)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
    }
    loaded.skills = query_string_column(
        connection,
        "SELECT skill_id FROM session_skills WHERE session_id=?1 ORDER BY skill_id",
        id,
    )?;
    loaded.failed_tool_results = query_string_column(
        connection,
        "SELECT tool_call_id FROM session_failed_tool_results WHERE session_id=?1 ORDER BY tool_call_id",
        id,
    )?;
    loaded.subagent_snapshots = {
        let mut statement = connection.prepare(
            "SELECT storage_key, snapshot_json FROM session_subagent_snapshots WHERE session_id=?1 ORDER BY storage_key",
        )?;
        statement
            .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };
    loaded.todo_state = connection
        .query_row(
            "SELECT revision, last_updated_by, updated_at FROM session_todo_state WHERE session_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    loaded.todos = {
        let mut statement = connection.prepare(
            "SELECT todo_id, content, status FROM session_todos WHERE session_id=?1 ORDER BY position",
        )?;
        statement
            .query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };
    loaded.pending_plan = connection
        .query_row(
            "SELECT plan_id, original_user_message_index, assistant_plan_message_index, created_at FROM session_pending_plans WHERE session_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    loaded.usage = connection
        .query_row(
            "SELECT total_input, total_output, current_input, current_output, input_source, output_source, current_day FROM session_usage WHERE session_id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
        )
        .optional()?;
    loaded.usage_days = {
        let mut statement = connection.prepare(
            "SELECT date, input, output FROM session_usage_days WHERE session_id=?1 ORDER BY date",
        )?;
        statement
            .query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };
    loaded.usage_labels = {
        let mut statement = connection.prepare(
            "SELECT scope, bucket, label, input, output FROM session_usage_labels WHERE session_id=?1 ORDER BY scope, bucket, label",
        )?;
        statement
            .query_map([id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(Some(loaded))
}

pub(super) fn validate_session_record(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<bool, StorageError> {
    load_session_record(connection, id)?
        .map(rebuild_session)
        .transpose()
        .map(|session| session.is_some())
}

fn query_string_column(
    connection: &rusqlite::Connection,
    sql: &str,
    id: &str,
) -> rusqlite::Result<Vec<String>> {
    let mut statement = connection.prepare(sql)?;
    statement
        .query_map([id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()
}

fn parse_optional_json<T: serde::de::DeserializeOwned>(
    value: Option<String>,
) -> Result<Option<T>, StorageError> {
    value
        .map(|json| {
            serde_json::from_str(&json).map_err(|error| StorageError::new(error.to_string()))
        })
        .transpose()
}

fn rebuild_session(loaded: LoadedSession) -> Result<Session, StorageError> {
    validate_persisted_session_identity(&loaded.id, &loaded.name)?;
    if loaded.version != i64::from(crate::SESSION_VERSION) {
        return Err(StorageError::new(format!(
            "Invalid persisted session version {}",
            loaded.version
        )));
    }
    if !matches!(
        loaded.think_level.as_str(),
        "auto" | "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
    ) {
        return Err(StorageError::new(format!(
            "Invalid persisted session think level '{}'",
            loaded.think_level
        )));
    }
    let show_react = parse_session_flag(loaded.show_react, "show_react")?;
    let show_tools = parse_session_flag(loaded.show_tools, "show_tools")?;
    let show_reasoning = parse_session_flag(loaded.show_reasoning, "show_reasoning")?;
    let messages = loaded
        .messages
        .into_iter()
        .enumerate()
        .map(|(expected_position, message)| {
            let expected_position = i64::try_from(expected_position)
                .map_err(|_| StorageError::new("Too many session messages"))?;
            if message.position != expected_position {
                return Err(StorageError::new(format!(
                    "Invalid session message position {}; expected {expected_position}",
                    message.position
                )));
            }
            let StoredMessage {
                position: _,
                role,
                content,
                images_json,
                thinking,
                thinking_blocks_json,
                tool_calls_json,
                tool_call_id,
                timestamp,
                fingerprint,
            } = message;
            let rebuilt = ChatMessage {
                role,
                content,
                images: parse_optional_json(images_json)?,
                thinking,
                anthropic_thinking_blocks: parse_optional_json(thinking_blocks_json)?,
                tool_calls: parse_optional_json(tool_calls_json)?,
                tool_call_id,
                timestamp: timestamp
                    .map(|value| to_u64(value, "message timestamp"))
                    .transpose()?,
            };
            let rebuilt_fingerprint = message_fingerprint(&rebuilt)?;
            if fingerprint != rebuilt_fingerprint {
                return Err(StorageError::new(format!(
                    "Session message fingerprint mismatch at position {expected_position}"
                )));
            }
            Ok(rebuilt)
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    let subagent_snapshots = loaded
        .subagent_snapshots
        .into_iter()
        .map(|(key, json)| {
            let snapshot: SubagentHistorySnapshot = serde_json::from_str(&json)
                .map_err(|error| StorageError::new(error.to_string()))?;
            Ok((key, snapshot))
        })
        .collect::<Result<HashMap<_, _>, StorageError>>()?;
    let todos = TodoSnapshot {
        revision: loaded
            .todo_state
            .as_ref()
            .map(|state| to_u64(state.0, "todo revision"))
            .transpose()?
            .unwrap_or_default(),
        items: loaded
            .todos
            .into_iter()
            .map(|(id, content, status)| {
                Ok(TodoItem {
                    id,
                    content,
                    status: parse_todo_status(&status)?,
                })
            })
            .collect::<Result<Vec<_>, StorageError>>()?,
        last_updated_by: loaded
            .todo_state
            .as_ref()
            .map(|state| parse_todo_updated_by(&state.1))
            .transpose()?
            .unwrap_or_default(),
        updated_at: loaded
            .todo_state
            .as_ref()
            .map(|state| to_u64(state.2, "todo updated_at"))
            .transpose()?
            .unwrap_or(loaded.updated_at.max(0) as u64),
    };
    let mut normalized_todos = todos.clone();
    crate::todos::normalize_snapshot(
        &mut normalized_todos,
        to_u64(loaded.updated_at, "session updated_at")?,
    );
    if normalized_todos != todos {
        return Err(StorageError::new(
            "Persisted Todo state is not in canonical form",
        ));
    }
    let pending_plan = loaded
        .pending_plan
        .map(|(id, user_index, assistant_index, created_at)| {
            Ok::<PendingPlan, StorageError>(PendingPlan {
                id,
                original_user_message_index: to_usize(user_index, "pending plan user index")?,
                assistant_plan_message_index: to_usize(
                    assistant_index,
                    "pending plan assistant index",
                )?,
                created_at: to_u64(created_at, "pending plan created_at")?,
            })
        })
        .transpose()?;
    let mut usage_history = loaded
        .usage_days
        .into_iter()
        .map(|(date, input, output)| {
            Ok(DailyUsageSnapshot {
                date,
                input: to_u64(input, "usage day input")?,
                output: to_u64(output, "usage day output")?,
                providers: HashMap::new(),
            })
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    let usage = loaded.usage.unwrap_or_else(|| {
        (
            0,
            0,
            0,
            0,
            crate::default_token_usage_source(),
            crate::default_token_usage_source(),
            String::new(),
        )
    });
    let mut daily_provider_usage = HashMap::new();
    let mut total_label_usage = HashMap::new();
    for (scope, bucket, label, input, output) in loaded.usage_labels {
        let values = [
            to_u64(input, "usage label input")?,
            to_u64(output, "usage label output")?,
        ];
        match scope.as_str() {
            "current" if bucket == usage.6 => {
                daily_provider_usage.insert(label, values);
            }
            "history" => {
                let Some(day) = usage_history.iter_mut().find(|day| day.date == bucket) else {
                    return Err(StorageError::new(format!(
                        "Usage label references missing history day '{bucket}'"
                    )));
                };
                day.providers.insert(label, values);
            }
            "total" if bucket.is_empty() => {
                total_label_usage.insert(label, values);
            }
            "current" | "total" => {
                return Err(StorageError::new(format!(
                    "Invalid usage label bucket '{bucket}' for scope '{scope}'"
                )));
            }
            _ => {
                return Err(StorageError::new(format!(
                    "Invalid usage label scope '{scope}'"
                )));
            }
        }
    }
    let mut session = Session {
        id: loaded.id,
        name: loaded.name,
        messages,
        created_at: to_u64(loaded.created_at, "session created_at")?,
        updated_at: to_u64(loaded.updated_at, "session updated_at")?,
        tool_calls_count: to_usize(loaded.tool_calls_count, "tool call count")?,
        input_tokens: to_u64(usage.0, "total input tokens")?,
        output_tokens: to_u64(usage.1, "total output tokens")?,
        daily_input_tokens: to_u64(usage.2, "daily input tokens")?,
        daily_output_tokens: to_u64(usage.3, "daily output tokens")?,
        input_token_source: usage.4,
        output_token_source: usage.5,
        token_usage_day: usage.6,
        daily_provider_usage,
        total_label_usage,
        usage_history,
        model_override: loaded.model_override,
        think_level: loaded.think_level,
        show_react,
        show_tools,
        show_reasoning,
        enabled_system_skills: loaded.skills.into_iter().collect::<HashSet<_>>(),
        disabled_system_skills: HashSet::new(),
        failed_tool_results: loaded.failed_tool_results.into_iter().collect(),
        subagent_snapshots,
        todos,
        pending_plan,
        version: u32::try_from(loaded.version)
            .map_err(|_| StorageError::new("Invalid session version"))?,
        workspace: crate::session_workspace_path(""),
    };
    session.workspace = crate::session_workspace_path(&session.id);
    crate::session_store::normalize_session(&mut session);
    Ok(session)
}
