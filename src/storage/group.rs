use std::collections::HashSet;

use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::session::canonical_session_id_record;
use super::{Database, StorageError};
use crate::session_group::{
    GroupMessage, GroupRun, GroupVote, SessionGroup, SessionGroupSummary, normalize_group,
};

fn to_i64(value: u64, field: &str) -> Result<i64, StorageError> {
    i64::try_from(value)
        .map_err(|_| StorageError::new(format!("{field} exceeds SQLite INTEGER range")))
}

fn to_u64(value: i64, field: &str) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::new(format!("Invalid negative {field}")))
}

fn to_usize(value: i64, field: &str) -> Result<usize, StorageError> {
    usize::try_from(value)
        .map_err(|_| StorageError::new(format!("Invalid negative or oversized {field}")))
}

#[derive(Default)]
struct LoadedGroup {
    id: String,
    name: String,
    created_at: i64,
    updated_at: i64,
    version: i64,
    members: Vec<(String, i64)>,
    votes: Vec<LoadedVote>,
    messages: Vec<LoadedMessage>,
    runs: Vec<LoadedRun>,
}

struct LoadedVote {
    id: String,
    action: String,
    target_session_id: String,
    requester_session_id: String,
    threshold: i64,
    created_at: i64,
    updated_at: i64,
    approvals: Vec<String>,
}

struct LoadedMessage {
    id: String,
    role: String,
    session_id: Option<String>,
    content: String,
    timestamp: i64,
    turn_id: Option<String>,
    run_id: Option<String>,
}

struct LoadedRun {
    id: String,
    session_id: String,
    status: String,
    prompt: String,
    result_excerpt: Option<String>,
    error: Option<String>,
    created_at: i64,
    updated_at: i64,
    completed_at: Option<i64>,
}

enum GroupSaveOutcome {
    Saved,
    MissingSessions(Vec<String>),
}

fn canonicalize_reference(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<String, StorageError> {
    if crate::is_main(id) {
        return Ok(crate::MAIN_SESSION_ID.to_string());
    }
    Ok(canonical_session_id_record(connection, id)?.unwrap_or_else(|| id.to_string()))
}

fn prepare_group_for_save(
    connection: &rusqlite::Connection,
    group: &SessionGroup,
) -> Result<(SessionGroup, Vec<String>), StorageError> {
    let mut group = group.clone();
    let mut missing_sessions = Vec::new();
    let mut seen_missing = HashSet::new();
    let mut members = Vec::new();
    for member in std::mem::take(&mut group.members) {
        let Ok(valid) = crate::session_store::validate_session_id(&member) else {
            continue;
        };
        if crate::is_main(valid) {
            continue;
        }
        match canonical_session_id_record(connection, valid)? {
            Some(canonical) => members.push(canonical),
            None => {
                let key = if cfg!(windows) {
                    valid.to_ascii_lowercase()
                } else {
                    valid.to_string()
                };
                if seen_missing.insert(key) {
                    missing_sessions.push(valid.to_string());
                }
            }
        }
    }
    group.members = members;
    group.admins = std::mem::take(&mut group.admins)
        .into_iter()
        .map(|id| canonicalize_reference(connection, &id))
        .collect::<Result<Vec<_>, _>>()?;
    for vote in &mut group.pending_votes {
        vote.target_session_id = canonicalize_reference(connection, &vote.target_session_id)?;
        vote.requester_session_id = canonicalize_reference(connection, &vote.requester_session_id)?;
        vote.approvals = std::mem::take(&mut vote.approvals)
            .into_iter()
            .map(|id| canonicalize_reference(connection, &id))
            .collect::<Result<Vec<_>, _>>()?;
    }
    for message in &mut group.messages {
        if let Some(session_id) = message.session_id.as_mut() {
            *session_id = canonicalize_reference(connection, session_id)?;
        }
    }
    for run in &mut group.runs {
        run.session_id = canonicalize_reference(connection, &run.session_id)?;
    }
    normalize_group(&mut group);
    Ok((group, missing_sessions))
}

fn missing_sessions_error(missing_sessions: &[String]) -> StorageError {
    StorageError::new(format!(
        "{}{}",
        super::GROUP_MISSING_SESSIONS_ERROR_PREFIX,
        missing_sessions.join(", ")
    ))
}

pub(super) fn save_group_record(
    connection: &rusqlite::Connection,
    group: &SessionGroup,
) -> Result<(), StorageError> {
    let (group, missing_sessions) = prepare_group_for_save(connection, group)?;
    if !missing_sessions.is_empty() {
        return Err(missing_sessions_error(&missing_sessions));
    }
    save_prepared_group_record(connection, &group)
}

fn save_prepared_group_record(
    connection: &rusqlite::Connection,
    group: &SessionGroup,
) -> Result<(), StorageError> {
    connection.execute(
        r#"INSERT INTO groups(id, name, created_at, updated_at, version)
           VALUES (?1, ?2, ?3, ?4, ?5)
           ON CONFLICT(id) DO UPDATE SET
             name=excluded.name,
             created_at=excluded.created_at,
             updated_at=excluded.updated_at,
             version=excluded.version"#,
        params![
            group.id,
            group.name,
            to_i64(group.created_at, "group created_at")?,
            to_i64(group.updated_at, "group updated_at")?,
            i64::from(group.version),
        ],
    )?;
    connection.execute("DELETE FROM group_members WHERE group_id=?1", [&group.id])?;
    let admin_set = group.admins.iter().collect::<HashSet<_>>();
    for (position, member) in group.members.iter().enumerate() {
        connection.execute(
            "INSERT INTO group_members(group_id, session_id, position, is_admin) VALUES (?1, ?2, ?3, ?4)",
            params![
                group.id,
                member,
                i64::try_from(position).map_err(|_| StorageError::new("Too many group members"))?,
                i64::from(admin_set.contains(member)),
            ],
        )?;
    }

    connection.execute("DELETE FROM group_votes WHERE group_id=?1", [&group.id])?;
    for (position, vote) in group.pending_votes.iter().enumerate() {
        connection.execute(
            r#"INSERT INTO group_votes(
                group_id, vote_id, action, target_session_id,
                requester_session_id, threshold, created_at, updated_at, position
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                group.id,
                vote.id,
                vote.action,
                vote.target_session_id,
                vote.requester_session_id,
                i64::try_from(vote.threshold)
                    .map_err(|_| StorageError::new("Invalid vote threshold"))?,
                to_i64(vote.created_at, "vote created_at")?,
                to_i64(vote.updated_at, "vote updated_at")?,
                i64::try_from(position).map_err(|_| StorageError::new("Too many group votes"))?,
            ],
        )?;
        for (approval_position, approval) in vote.approvals.iter().enumerate() {
            connection.execute(
                "INSERT INTO group_vote_approvals(group_id, vote_id, position, session_id) VALUES (?1, ?2, ?3, ?4)",
                params![
                    group.id,
                    vote.id,
                    i64::try_from(approval_position)
                        .map_err(|_| StorageError::new("Too many vote approvals"))?,
                    approval,
                ],
            )?;
        }
    }

    connection.execute("DELETE FROM group_messages WHERE group_id=?1", [&group.id])?;
    for (position, message) in group.messages.iter().enumerate() {
        connection.execute(
            r#"INSERT INTO group_messages(
                group_id, message_id, position, role, session_id,
                content, timestamp, turn_id, run_id
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"#,
            params![
                group.id,
                message.id,
                i64::try_from(position)
                    .map_err(|_| StorageError::new("Too many group messages"))?,
                message.role,
                message.session_id,
                message.content,
                to_i64(message.timestamp, "group message timestamp")?,
                message.turn_id,
                message.run_id,
            ],
        )?;
    }

    connection.execute("DELETE FROM group_runs WHERE group_id=?1", [&group.id])?;
    for (position, run) in group.runs.iter().enumerate() {
        connection.execute(
            r#"INSERT INTO group_runs(
                group_id, run_id, position, session_id, status, prompt,
                result_excerpt, error, created_at, updated_at, completed_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
            params![
                group.id,
                run.id,
                i64::try_from(position).map_err(|_| StorageError::new("Too many group runs"))?,
                run.session_id,
                run.status,
                run.prompt,
                run.result_excerpt,
                run.error,
                to_i64(run.created_at, "group run created_at")?,
                to_i64(run.updated_at, "group run updated_at")?,
                run.completed_at
                    .map(|value| to_i64(value, "group run completed_at"))
                    .transpose()?,
            ],
        )?;
    }
    Ok(())
}

fn canonical_group_id_record(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<Option<String>, StorageError> {
    let mut canonical = connection
        .query_row("SELECT id FROM groups WHERE id=?1", [id], |row| row.get(0))
        .optional()?;
    if canonical.is_none() && cfg!(windows) {
        canonical = connection
            .query_row(
                "SELECT id FROM groups WHERE id=?1 COLLATE NOCASE ORDER BY id LIMIT 1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
    }
    if let Some(canonical) = canonical.as_deref() {
        validate_persisted_group_id(canonical)?;
    }
    Ok(canonical)
}

impl Database {
    pub(crate) async fn save_group(&self, group: &SessionGroup) -> Result<(), StorageError> {
        let group = group.clone();
        let outcome = self
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let (group, missing_sessions) = prepare_group_for_save(&transaction, &group)?;
                if !missing_sessions.is_empty() {
                    return Ok(GroupSaveOutcome::MissingSessions(missing_sessions));
                }
                save_prepared_group_record(&transaction, &group)?;
                transaction.commit()?;
                Ok(GroupSaveOutcome::Saved)
            })
            .await?;
        match outcome {
            GroupSaveOutcome::Saved => Ok(()),
            GroupSaveOutcome::MissingSessions(missing_sessions) => {
                Err(missing_sessions_error(&missing_sessions))
            }
        }
    }
    #[cfg(test)]
    pub(crate) async fn load_group(&self, id: &str) -> Result<Option<SessionGroup>, StorageError> {
        let id = id.to_string();
        self.read(move |connection| {
            let Some(id) = canonical_group_id_record(connection, &id)? else {
                return Ok(None);
            };
            load_group_record(connection, &id)?
                .map(rebuild_group)
                .transpose()
        })
        .await
    }

    #[cfg(not(test))]
    pub(crate) fn load_group_blocking(
        &self,
        id: &str,
    ) -> Result<Option<SessionGroup>, StorageError> {
        let id = id.to_string();
        self.blocking_read(move |connection| {
            load_group_record(connection, &id)?
                .map(rebuild_group)
                .transpose()
        })
    }

    #[cfg(not(test))]
    pub(crate) fn canonical_group_id_blocking(
        &self,
        id: &str,
    ) -> Result<Option<String>, StorageError> {
        let id = id.to_string();
        self.blocking_read(move |connection| canonical_group_id_record(connection, &id))
    }

    #[cfg(not(test))]
    pub(crate) fn list_group_summaries_blocking(
        &self,
    ) -> Result<Vec<SessionGroupSummary>, StorageError> {
        self.blocking_read(query_group_summaries)
    }

    #[cfg(test)]
    pub(crate) async fn list_group_summaries(
        &self,
    ) -> Result<Vec<SessionGroupSummary>, StorageError> {
        self.read(query_group_summaries).await
    }

    pub(crate) async fn list_group_ids(&self) -> Result<Vec<String>, StorageError> {
        self.read(|connection| {
            let mut statement = connection.prepare("SELECT id FROM groups ORDER BY id")?;
            let ids = statement
                .query_map([], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            for id in &ids {
                validate_persisted_group_id(id)?;
            }
            Ok(ids)
        })
        .await
    }

    #[cfg(not(test))]
    pub(crate) async fn delete_group(&self, id: &str) -> Result<bool, StorageError> {
        let id = id.to_string();
        self.call(move |connection| {
            Ok(connection.execute("DELETE FROM groups WHERE id=?1", [&id])? > 0)
        })
        .await
    }
}

fn query_group_summaries(
    connection: &mut rusqlite::Connection,
) -> Result<Vec<SessionGroupSummary>, StorageError> {
    let mut statement = connection.prepare(
        r#"SELECT g.id, g.name, g.version,
                  (SELECT COUNT(*) FROM group_members m WHERE m.group_id=g.id),
                  (SELECT COUNT(*) FROM group_messages gm WHERE gm.group_id=g.id),
                  (SELECT COUNT(*) FROM group_runs gr WHERE gr.group_id=g.id AND gr.status IN ('queued','running')),
                  g.created_at, g.updated_at
           FROM groups g ORDER BY g.updated_at DESC, g.id"#,
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(id, name, version, members, messages, running, created_at, updated_at)| {
                validate_persisted_group_identity(&id, &name, version)?;
                Ok(SessionGroupSummary {
                    id,
                    name,
                    members: to_usize(members, "group member count")?,
                    messages: to_usize(messages, "group message count")?,
                    running: to_usize(running, "active group run count")?,
                    created_at: to_u64(created_at, "group created_at")?,
                    updated_at: to_u64(updated_at, "group updated_at")?,
                    corrupt: false,
                })
            },
        )
        .collect()
}

fn validate_persisted_group_id(id: &str) -> Result<(), StorageError> {
    let canonical_id = crate::session_group::validate_group_id(id)
        .map_err(|error| StorageError::new(format!("Invalid persisted group id: {error}")))?;
    if canonical_id != id {
        return Err(StorageError::new(
            "Persisted group id is not in canonical form",
        ));
    }
    Ok(())
}

fn validate_persisted_group_identity(
    id: &str,
    name: &str,
    version: i64,
) -> Result<(), StorageError> {
    validate_persisted_group_id(id)?;
    let normalized_name = crate::session_group::validate_group_name(name)
        .map_err(|error| StorageError::new(format!("Invalid persisted group name: {error}")))?;
    if normalized_name != name {
        return Err(StorageError::new(
            "Persisted group name is not in canonical form",
        ));
    }
    if version != i64::from(crate::session_group::GROUP_VERSION) {
        return Err(StorageError::new(format!(
            "Invalid persisted group version {version}"
        )));
    }
    Ok(())
}

fn load_group_record(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<Option<LoadedGroup>, StorageError> {
    let Some(mut group) = connection
        .query_row(
            "SELECT id, name, created_at, updated_at, version FROM groups WHERE id=?1",
            [id],
            |row| {
                Ok(LoadedGroup {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: row.get(2)?,
                    updated_at: row.get(3)?,
                    version: row.get(4)?,
                    ..LoadedGroup::default()
                })
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    group.members = {
        let mut statement = connection.prepare(
            "SELECT session_id, is_admin FROM group_members WHERE group_id=?1 ORDER BY position",
        )?;
        statement
            .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?
    };
    group.votes = {
        let mut statement = connection.prepare(
            r#"SELECT vote_id, action, target_session_id, requester_session_id,
                      threshold, created_at, updated_at
               FROM group_votes WHERE group_id=?1 ORDER BY position"#,
        )?;
        let mut votes = statement
            .query_map([id], |row| {
                Ok(LoadedVote {
                    id: row.get(0)?,
                    action: row.get(1)?,
                    target_session_id: row.get(2)?,
                    requester_session_id: row.get(3)?,
                    threshold: row.get(4)?,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                    approvals: Vec::new(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for vote in &mut votes {
            let mut approvals = connection.prepare(
                "SELECT session_id FROM group_vote_approvals WHERE group_id=?1 AND vote_id=?2 ORDER BY position",
            )?;
            vote.approvals = approvals
                .query_map(params![id, vote.id], |row| row.get(0))?
                .collect::<Result<Vec<_>, _>>()?;
        }
        votes
    };
    group.messages = {
        let mut statement = connection.prepare(
            r#"SELECT message_id, role, session_id, content, timestamp, turn_id, run_id
               FROM group_messages WHERE group_id=?1 ORDER BY position"#,
        )?;
        statement
            .query_map([id], |row| {
                Ok(LoadedMessage {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    session_id: row.get(2)?,
                    content: row.get(3)?,
                    timestamp: row.get(4)?,
                    turn_id: row.get(5)?,
                    run_id: row.get(6)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    group.runs = {
        let mut statement = connection.prepare(
            r#"SELECT run_id, session_id, status, prompt, result_excerpt, error,
                      created_at, updated_at, completed_at
               FROM group_runs WHERE group_id=?1 ORDER BY position"#,
        )?;
        statement
            .query_map([id], |row| {
                Ok(LoadedRun {
                    id: row.get(0)?,
                    session_id: row.get(1)?,
                    status: row.get(2)?,
                    prompt: row.get(3)?,
                    result_excerpt: row.get(4)?,
                    error: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                    completed_at: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(Some(group))
}

pub(super) fn validate_group_record(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<bool, StorageError> {
    load_group_record(connection, id)?
        .map(rebuild_group)
        .transpose()
        .map(|group| group.is_some())
}

fn rebuild_group(loaded: LoadedGroup) -> Result<SessionGroup, StorageError> {
    validate_persisted_group_identity(&loaded.id, &loaded.name, loaded.version)?;
    for (_, is_admin) in &loaded.members {
        if !matches!(*is_admin, 0 | 1) {
            return Err(StorageError::new(format!(
                "Invalid group member administrator flag '{is_admin}'"
            )));
        }
    }
    for run in &loaded.runs {
        if !matches!(
            run.status.as_str(),
            "queued" | "running" | "completed" | "failed" | "stopped"
        ) {
            return Err(StorageError::new(format!(
                "Invalid group run status '{}'",
                run.status
            )));
        }
    }
    let members = loaded
        .members
        .iter()
        .map(|(member, _)| member.clone())
        .collect::<Vec<_>>();
    let admins = loaded
        .members
        .iter()
        .filter(|(_, is_admin)| *is_admin == 1)
        .map(|(member, _)| member.clone())
        .collect::<Vec<_>>();
    let pending_votes = loaded
        .votes
        .into_iter()
        .map(|vote| {
            Ok(GroupVote {
                id: vote.id,
                action: vote.action,
                target_session_id: vote.target_session_id,
                requester_session_id: vote.requester_session_id,
                approvals: vote.approvals,
                threshold: to_usize(vote.threshold, "vote threshold")?,
                created_at: to_u64(vote.created_at, "vote created_at")?,
                updated_at: to_u64(vote.updated_at, "vote updated_at")?,
            })
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    let messages = loaded
        .messages
        .into_iter()
        .map(|message| {
            Ok(GroupMessage {
                id: message.id,
                role: message.role,
                session_id: message.session_id,
                content: message.content,
                timestamp: to_u64(message.timestamp, "group message timestamp")?,
                turn_id: message.turn_id,
                run_id: message.run_id,
            })
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    let group_id = loaded.id.clone();
    let runs = loaded
        .runs
        .into_iter()
        .map(|run| {
            Ok(GroupRun {
                id: run.id,
                group_id: group_id.clone(),
                session_id: run.session_id,
                status: run.status,
                prompt: run.prompt,
                result_excerpt: run.result_excerpt,
                error: run.error,
                created_at: to_u64(run.created_at, "group run created_at")?,
                updated_at: to_u64(run.updated_at, "group run updated_at")?,
                completed_at: run
                    .completed_at
                    .map(|value| to_u64(value, "group run completed_at"))
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>, StorageError>>()?;
    let mut group = SessionGroup {
        id: loaded.id,
        name: loaded.name,
        members,
        admins,
        pending_votes,
        messages,
        runs,
        created_at: to_u64(loaded.created_at, "group created_at")?,
        updated_at: to_u64(loaded.updated_at, "group updated_at")?,
        version: u32::try_from(loaded.version)
            .map_err(|_| StorageError::new("Invalid group version"))?,
    };
    let original_members = group.members.clone();
    let original_admins = group.admins.clone();
    let original_votes = group.pending_votes.clone();
    normalize_group(&mut group);
    if group.members != original_members
        || group.admins != original_admins
        || group.pending_votes != original_votes
    {
        return Err(StorageError::new(
            "Persisted group membership or vote state is not in canonical form",
        ));
    }
    Ok(group)
}
