use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Database, StorageError};
use crate::{Session, session_group::SessionGroup};

const MIGRATION_METADATA_KEY: &str = "legacy_json_migration";
const MIGRATION_JOURNAL_FILE: &str = "sqlite-migration.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MigrationFile {
    kind: String,
    id: String,
    source: String,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MigrationManifest {
    version: u32,
    created_at: u64,
    sessions: usize,
    groups: usize,
    files: Vec<MigrationFile>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MigrationJournal {
    version: u32,
    phase: String,
    backup_dir: PathBuf,
    had_sessions_dir: bool,
    had_groups_dir: bool,
    manifest: MigrationManifest,
}

struct Candidate<T> {
    id: String,
    value: T,
    path: PathBuf,
    modified: SystemTime,
    is_tmp: bool,
    sha256: String,
}

fn normalized_id(id: &str) -> String {
    if cfg!(windows) {
        id.to_ascii_lowercase()
    } else {
        id.to_string()
    }
}

fn ids_match(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

fn legacy_file_id(path: &Path) -> Option<(String, bool)> {
    let name = path.file_name()?.to_str()?;
    if let Some(id) = name.strip_suffix(".json.tmp") {
        return Some((id.to_string(), true));
    }
    name.strip_suffix(".json").map(|id| (id.to_string(), false))
}

pub(crate) fn preflight_legacy_storage_path_conflicts(
    database_path: &Path,
) -> Result<(), StorageError> {
    let home = database_path
        .parent()
        .ok_or_else(|| StorageError::new("SQLite database has no parent directory"))?;
    let sessions_dir = home.join("sessions");
    if !sessions_dir.exists() {
        return Ok(());
    }
    let entries = std::fs::read_dir(&sessions_dir).map_err(|error| {
        StorageError::new(format!(
            "Failed to inspect legacy sessions directory {}: {error}",
            sessions_dir.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            StorageError::new(format!(
                "Failed to inspect legacy sessions directory {}: {error}",
                sessions_dir.display()
            ))
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some((file_id, _)) = legacy_file_id(&path) else {
            continue;
        };
        if crate::session_store::is_storage_owned_session_id(&file_id) {
            return Err(StorageError::new(format!(
                "Legacy session file {} uses Session id '{}' which conflicts with LingClaw SQLite storage. Rename or remove this legacy Session with the previous LingClaw version before upgrading.",
                path.display(),
                file_id
            )));
        }
    }
    Ok(())
}

fn file_modified(path: &Path) -> Result<SystemTime, StorageError> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| {
            StorageError::new(format!(
                "Failed to inspect legacy storage file {}: {error}",
                path.display()
            ))
        })
}

fn read_legacy_file(path: &Path) -> Result<Vec<u8>, StorageError> {
    std::fs::read(path).map_err(|error| {
        StorageError::new(format!(
            "Failed to read legacy storage file {}: {error}",
            path.display()
        ))
    })
}

fn select_recoverable_candidates<T, F>(
    mut candidates: HashMap<String, Vec<(String, PathBuf, bool)>>,
    mut load: F,
) -> Result<Vec<Candidate<T>>, StorageError>
where
    F: FnMut(&str, &Path, bool) -> Result<Candidate<T>, StorageError>,
{
    let mut selected = Vec::with_capacity(candidates.len());
    for (_, paths) in candidates.drain() {
        let mut valid = Vec::new();
        let mut errors = Vec::new();
        for (file_id, path, is_tmp) in paths {
            match load(&file_id, &path, is_tmp) {
                Ok(candidate) => valid.push(candidate),
                Err(error) => errors.push(error.to_string()),
            }
        }
        let Some(candidate) = valid.into_iter().max_by(|left, right| {
            left.modified
                .cmp(&right.modified)
                .then_with(|| left.is_tmp.cmp(&right.is_tmp))
        }) else {
            return Err(StorageError::new(errors.join("; ")));
        };
        selected.push(candidate);
    }
    selected.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(selected)
}

fn load_legacy_sessions(dir: &Path) -> Result<Vec<Candidate<Session>>, StorageError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(dir).map_err(|error| {
        StorageError::new(format!(
            "Failed to inspect legacy sessions directory {}: {error}",
            dir.display()
        ))
    })?;
    let mut candidates = HashMap::<String, Vec<(String, PathBuf, bool)>>::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            StorageError::new(format!(
                "Failed to inspect legacy sessions directory {}: {error}",
                dir.display()
            ))
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some((file_id, is_tmp)) = legacy_file_id(&path) else {
            continue;
        };
        let canonical_file_id =
            crate::session_store::validate_session_id(&file_id).map_err(|error| {
                StorageError::new(format!(
                    "Invalid legacy session file {}: {error}",
                    path.display()
                ))
            })?;
        if canonical_file_id != file_id {
            return Err(StorageError::new(format!(
                "Invalid legacy session file {}: session id '{}' is not in canonical form",
                path.display(),
                file_id
            )));
        }
        candidates
            .entry(normalized_id(&file_id))
            .or_default()
            .push((file_id, path, is_tmp));
    }
    select_recoverable_candidates(candidates, |file_id, path, is_tmp| {
        let bytes = read_legacy_file(path)?;
        let mut session = serde_json::from_slice::<Session>(&bytes).map_err(|error| {
            StorageError::new(format!(
                "Corrupt legacy session file {}: {error}",
                path.display()
            ))
        })?;
        let canonical_session_id =
            crate::session_store::validate_session_id(&session.id).map_err(|error| {
                StorageError::new(format!(
                    "Invalid session id inside {}: {error}",
                    path.display()
                ))
            })?;
        if canonical_session_id != session.id {
            return Err(StorageError::new(format!(
                "Invalid session id inside {}: '{}' is not in canonical form",
                path.display(),
                session.id
            )));
        }
        if !ids_match(file_id, &session.id) {
            return Err(StorageError::new(format!(
                "Legacy session file {} contains mismatched id '{}'",
                path.display(),
                session.id
            )));
        }
        crate::session_store::normalize_session(&mut session);
        session.workspace = crate::session_workspace_path(&session.id);
        Ok(Candidate {
            id: session.id.clone(),
            value: session,
            modified: file_modified(path)?,
            is_tmp,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            path: path.to_path_buf(),
        })
    })
}

fn load_legacy_groups(dir: &Path) -> Result<Vec<Candidate<SessionGroup>>, StorageError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let entries = std::fs::read_dir(dir).map_err(|error| {
        StorageError::new(format!(
            "Failed to inspect legacy groups directory {}: {error}",
            dir.display()
        ))
    })?;
    let mut candidates = HashMap::<String, Vec<(String, PathBuf, bool)>>::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            StorageError::new(format!(
                "Failed to inspect legacy groups directory {}: {error}",
                dir.display()
            ))
        })?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some((file_id, is_tmp)) = legacy_file_id(&path) else {
            continue;
        };
        let canonical_file_id =
            crate::session_group::validate_group_id(&file_id).map_err(|error| {
                StorageError::new(format!(
                    "Invalid legacy group file {}: {error}",
                    path.display()
                ))
            })?;
        if canonical_file_id != file_id {
            return Err(StorageError::new(format!(
                "Invalid legacy group file {}: group id '{}' is not in canonical form",
                path.display(),
                file_id
            )));
        }
        candidates
            .entry(normalized_id(&file_id))
            .or_default()
            .push((file_id, path, is_tmp));
    }
    select_recoverable_candidates(candidates, |file_id, path, is_tmp| {
        let bytes = read_legacy_file(path)?;
        let mut group = serde_json::from_slice::<SessionGroup>(&bytes).map_err(|error| {
            StorageError::new(format!(
                "Corrupt legacy group file {}: {error}",
                path.display()
            ))
        })?;
        let canonical_group_id =
            crate::session_group::validate_group_id(&group.id).map_err(|error| {
                StorageError::new(format!(
                    "Invalid group id inside {}: {error}",
                    path.display()
                ))
            })?;
        if canonical_group_id != group.id {
            return Err(StorageError::new(format!(
                "Invalid group id inside {}: '{}' is not in canonical form",
                path.display(),
                group.id
            )));
        }
        if !ids_match(file_id, &group.id) {
            return Err(StorageError::new(format!(
                "Legacy group file {} contains mismatched id '{}'",
                path.display(),
                group.id
            )));
        }
        validate_legacy_group_live_state(&group, path)?;
        crate::session_group::normalize_group(&mut group);
        Ok(Candidate {
            id: group.id.clone(),
            value: group,
            modified: file_modified(path)?,
            is_tmp,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            path: path.to_path_buf(),
        })
    })
}

fn validate_legacy_group_live_state(group: &SessionGroup, path: &Path) -> Result<(), StorageError> {
    fn invalid(path: &Path, detail: impl std::fmt::Display) -> StorageError {
        StorageError::new(format!(
            "Invalid live group data in {}: {detail}",
            path.display()
        ))
    }

    fn validated_ids(
        values: &[String],
        field: &str,
        path: &Path,
    ) -> Result<Vec<String>, StorageError> {
        let mut seen = HashSet::new();
        let mut validated = Vec::with_capacity(values.len());
        for value in values {
            let valid = crate::session_store::validate_session_id(value)
                .map_err(|error| invalid(path, format!("{field} '{value}': {error}")))?;
            let key = normalized_id(valid);
            if !seen.insert(key.clone()) {
                return Err(invalid(
                    path,
                    format!("{field} contains duplicate session id '{valid}'"),
                ));
            }
            validated.push(key);
        }
        Ok(validated)
    }

    let members = validated_ids(&group.members, "members", path)?;
    let member_set = members
        .into_iter()
        .filter(|member| !crate::is_main(member))
        .collect::<HashSet<_>>();
    let admins = validated_ids(&group.admins, "admins", path)?;
    let admin_set = admins
        .into_iter()
        .filter(|admin| !crate::is_main(admin))
        .map(|admin| {
            if !member_set.contains(&admin) {
                Err(invalid(
                    path,
                    format!("admin '{admin}' is not a current group member"),
                ))
            } else {
                Ok(admin)
            }
        })
        .collect::<Result<HashSet<_>, _>>()?;

    let mut vote_ids = HashSet::new();
    for vote in &group.pending_votes {
        if vote.action != "remove_member" {
            return Err(invalid(
                path,
                format!(
                    "vote '{}' has unsupported action '{}'",
                    vote.id, vote.action
                ),
            ));
        }
        if !vote_ids.insert(vote.id.clone()) {
            return Err(invalid(
                path,
                format!("pending_votes contains duplicate vote id '{}'", vote.id),
            ));
        }
        if vote.threshold == 0 {
            return Err(invalid(
                path,
                format!("vote '{}' has a zero threshold", vote.id),
            ));
        }
        let target = crate::session_store::validate_session_id(&vote.target_session_id).map_err(
            |error| {
                invalid(
                    path,
                    format!(
                        "vote '{}' target '{}': {error}",
                        vote.id, vote.target_session_id
                    ),
                )
            },
        )?;
        if !member_set.contains(&normalized_id(target)) {
            return Err(invalid(
                path,
                format!(
                    "vote '{}' target '{}' is not a current group member",
                    vote.id, target
                ),
            ));
        }
        crate::session_store::validate_session_id(&vote.requester_session_id).map_err(|error| {
            invalid(
                path,
                format!(
                    "vote '{}' requester '{}': {error}",
                    vote.id, vote.requester_session_id
                ),
            )
        })?;
        let approvals = validated_ids(&vote.approvals, "vote approvals", path)?;
        for approval in approvals {
            if !admin_set.contains(&approval) {
                return Err(invalid(
                    path,
                    format!(
                        "vote '{}' approval '{}' is not a current group admin",
                        vote.id, approval
                    ),
                ));
            }
        }
    }

    let mut normalized = group.clone();
    crate::session_group::normalize_group(&mut normalized);
    if normalized.members != group.members
        || normalized.admins != group.admins
        || normalized.pending_votes != group.pending_votes
    {
        return Err(invalid(
            path,
            "normalization would discard or rewrite live members, admins, or pending votes",
        ));
    }
    Ok(())
}

fn validate_references(
    sessions: &[Candidate<Session>],
    groups: &[Candidate<SessionGroup>],
) -> Result<(), StorageError> {
    let session_ids = sessions
        .iter()
        .map(|candidate| normalized_id(&candidate.id))
        .collect::<HashSet<_>>();
    for group in groups {
        for member in &group.value.members {
            if !session_ids.contains(&normalized_id(member)) {
                return Err(StorageError::new(format!(
                    "Legacy group file {} references missing session '{}'",
                    group.path.display(),
                    member
                )));
            }
        }
    }
    Ok(())
}

fn validate_sqlite_import(
    sessions: &[Candidate<Session>],
    groups: &[Candidate<SessionGroup>],
) -> Result<(), StorageError> {
    let mut connection = rusqlite::Connection::open_in_memory()
        .map_err(|error| StorageError::new(error.to_string()))?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|error| StorageError::new(error.to_string()))?;
    connection
        .execute_batch(super::schema::INITIAL_SCHEMA)
        .map_err(|error| StorageError::new(error.to_string()))?;
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|error| StorageError::new(error.to_string()))?;

    for candidate in sessions {
        super::session::save_session_record(&transaction, &candidate.value).map_err(|error| {
            StorageError::new(format!(
                "Legacy session file {} cannot be imported into SQLite: {error}",
                candidate.path.display()
            ))
        })?;
    }
    for candidate in sessions {
        match super::session::validate_session_record(&transaction, &candidate.id) {
            Ok(true) => {}
            Ok(false) => {
                return Err(StorageError::new(format!(
                    "Legacy session file {} was not found after the SQLite import check",
                    candidate.path.display()
                )));
            }
            Err(error) => {
                return Err(StorageError::new(format!(
                    "Legacy session file {} cannot be restored after the SQLite import check: {error}",
                    candidate.path.display()
                )));
            }
        }
    }

    for candidate in groups {
        super::group::save_group_record(&transaction, &candidate.value).map_err(|error| {
            StorageError::new(format!(
                "Legacy group file {} cannot be imported into SQLite: {error}",
                candidate.path.display()
            ))
        })?;
    }
    for candidate in groups {
        match super::group::validate_group_record(&transaction, &candidate.id) {
            Ok(true) => {}
            Ok(false) => {
                return Err(StorageError::new(format!(
                    "Legacy group file {} was not found after the SQLite import check",
                    candidate.path.display()
                )));
            }
            Err(error) => {
                return Err(StorageError::new(format!(
                    "Legacy group file {} cannot be restored after the SQLite import check: {error}",
                    candidate.path.display()
                )));
            }
        }
    }

    transaction
        .rollback()
        .map_err(|error| StorageError::new(error.to_string()))
}

fn manifest_for(
    home: &Path,
    sessions: &[Candidate<Session>],
    groups: &[Candidate<SessionGroup>],
) -> MigrationManifest {
    let mut files = sessions
        .iter()
        .map(|candidate| MigrationFile {
            kind: "session".to_string(),
            id: candidate.id.clone(),
            source: candidate
                .path
                .strip_prefix(home)
                .unwrap_or(&candidate.path)
                .to_string_lossy()
                .replace('\\', "/"),
            sha256: candidate.sha256.clone(),
        })
        .chain(groups.iter().map(|candidate| {
            MigrationFile {
                kind: "group".to_string(),
                id: candidate.id.clone(),
                source: candidate
                    .path
                    .strip_prefix(home)
                    .unwrap_or(&candidate.path)
                    .to_string_lossy()
                    .replace('\\', "/"),
                sha256: candidate.sha256.clone(),
            }
        }))
        .collect::<Vec<_>>();
    files.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.id.cmp(&right.id))
    });
    MigrationManifest {
        version: 1,
        created_at: crate::now_epoch(),
        sessions: sessions.len(),
        groups: groups.len(),
        files,
    }
}

async fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), StorageError> {
    let data =
        serde_json::to_vec_pretty(value).map_err(|error| StorageError::new(error.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    tokio::fs::write(&tmp, data).await.map_err(|error| {
        StorageError::new(format!("Failed to write {}: {error}", tmp.display()))
    })?;
    if path.exists() {
        crate::session_store::replace_session_file_from_temp(path, &tmp).map_err(StorageError::new)
    } else {
        tokio::fs::rename(&tmp, path).await.map_err(|error| {
            StorageError::new(format!("Failed to finalize {}: {error}", path.display()))
        })
    }
}

fn migration_journal_tmp_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn migration_journal_backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.lingclaw-save-backup")
}

fn migration_journal_recovery_tmp_path(path: &Path) -> PathBuf {
    path.with_extension("json.recovery.tmp")
}

fn migration_journal_recovery_backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.recovery-backup")
}

fn validate_migration_journal(
    home: &Path,
    path: &Path,
    journal: &MigrationJournal,
) -> Result<(), StorageError> {
    if journal.version != 1 || !matches!(journal.phase.as_str(), "prepared" | "moved") {
        return Err(StorageError::new(format!(
            "Unsupported migration journal state in {}",
            path.display()
        )));
    }
    validate_migration_backup_dir(home, &journal.backup_dir)
}

fn read_migration_journal(home: &Path, path: &Path) -> Result<MigrationJournal, StorageError> {
    let data = std::fs::read(path).map_err(|error| {
        StorageError::new(format!(
            "Failed to read migration journal {}: {error}",
            path.display()
        ))
    })?;
    let journal = serde_json::from_slice::<MigrationJournal>(&data).map_err(|error| {
        StorageError::new(format!(
            "Corrupt migration journal {}: {error}",
            path.display()
        ))
    })?;
    validate_migration_journal(home, path, &journal)?;
    Ok(journal)
}

async fn restore_migration_journal(
    path: &Path,
    journal: &MigrationJournal,
) -> Result<(), StorageError> {
    let recovery_tmp = migration_journal_recovery_tmp_path(path);
    let recovery_backup = migration_journal_recovery_backup_path(path);
    for stale in [&recovery_tmp, &recovery_backup] {
        match tokio::fs::remove_file(stale).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StorageError::new(format!(
                    "Failed to remove stale migration recovery file {}: {error}",
                    stale.display()
                )));
            }
        }
    }
    let data =
        serde_json::to_vec_pretty(journal).map_err(|error| StorageError::new(error.to_string()))?;
    tokio::fs::write(&recovery_tmp, data)
        .await
        .map_err(|error| {
            StorageError::new(format!(
                "Failed to write migration recovery file {}: {error}",
                recovery_tmp.display()
            ))
        })?;

    let moved_existing = if path.exists() {
        tokio::fs::rename(path, &recovery_backup)
            .await
            .map_err(|error| {
                StorageError::new(format!(
                    "Failed to preserve migration journal {}: {error}",
                    path.display()
                ))
            })?;
        true
    } else {
        false
    };
    if let Err(error) = tokio::fs::rename(&recovery_tmp, path).await {
        if moved_existing {
            let _ = tokio::fs::rename(&recovery_backup, path).await;
        }
        return Err(StorageError::new(format!(
            "Failed to restore migration journal {}: {error}",
            path.display()
        )));
    }
    if moved_existing {
        let _ = tokio::fs::remove_file(&recovery_backup).await;
    }
    Ok(())
}

async fn cleanup_migration_journal_artifacts(path: &Path) -> Result<(), StorageError> {
    for artifact in [
        path.to_path_buf(),
        migration_journal_tmp_path(path),
        migration_journal_backup_path(path),
        migration_journal_recovery_tmp_path(path),
        migration_journal_recovery_backup_path(path),
    ] {
        match tokio::fs::remove_file(&artifact).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StorageError::new(format!(
                    "Failed to remove migration journal artifact {}: {error}",
                    artifact.display()
                )));
            }
        }
    }
    Ok(())
}

async fn recover_migration_journal(
    home: &Path,
    path: &Path,
) -> Result<Option<MigrationJournal>, StorageError> {
    let candidates = [
        (path.to_path_buf(), 1_u8),
        (migration_journal_tmp_path(path), 2_u8),
        (migration_journal_backup_path(path), 0_u8),
    ];
    let mut valid = Vec::new();
    let mut errors = Vec::new();
    for (candidate_path, source_rank) in candidates {
        if !candidate_path.exists() {
            continue;
        }
        match read_migration_journal(home, &candidate_path) {
            Ok(journal) => {
                let phase_rank = u8::from(journal.phase == "moved");
                valid.push((
                    journal,
                    candidate_path.clone(),
                    phase_rank,
                    file_modified(&candidate_path)?,
                    source_rank,
                ));
            }
            Err(error) => errors.push(error.to_string()),
        }
    }
    let Some((journal, source, _, _, _)) = valid.into_iter().max_by(
        |(_, _, left_phase, left_modified, left_source),
         (_, _, right_phase, right_modified, right_source)| {
            left_phase
                .cmp(right_phase)
                .then_with(|| left_modified.cmp(right_modified))
                .then_with(|| left_source.cmp(right_source))
        },
    ) else {
        if errors.is_empty() {
            return Ok(None);
        }
        return Err(StorageError::new(errors.join("; ")));
    };

    if source != path {
        restore_migration_journal(path, &journal).await?;
        let restored = read_migration_journal(home, path)?;
        if restored.phase != journal.phase || restored.backup_dir != journal.backup_dir {
            return Err(StorageError::new(format!(
                "Restored migration journal {} did not match the selected recovery state",
                path.display()
            )));
        }
    }
    for stale in [
        migration_journal_tmp_path(path),
        migration_journal_backup_path(path),
    ] {
        match tokio::fs::remove_file(&stale).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StorageError::new(format!(
                    "Failed to remove stale migration journal {}: {error}",
                    stale.display()
                )));
            }
        }
    }
    Ok(Some(journal))
}

fn next_backup_dir(home: &Path) -> PathBuf {
    let backups = home.join("backups");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    for suffix in 0..1000_u32 {
        let name = if suffix == 0 {
            format!("sqlite-migration-{stamp}")
        } else {
            format!("sqlite-migration-{stamp}-{suffix}")
        };
        let candidate = backups.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    backups.join(format!("sqlite-migration-{stamp}-overflow"))
}

async fn find_migration_backup_dirs(home: &Path) -> Result<Vec<PathBuf>, StorageError> {
    let backup_root = home.join("backups");
    let mut entries = match tokio::fs::read_dir(&backup_root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(StorageError::new(format!(
                "Failed to inspect the migration backup directory {}: {error}",
                backup_root.display()
            )));
        }
    };
    let mut backups = Vec::new();
    while let Some(entry) = entries.next_entry().await.map_err(|error| {
        StorageError::new(format!(
            "Failed to inspect the migration backup directory {}: {error}",
            backup_root.display()
        ))
    })? {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("sqlite-migration-") {
            continue;
        }
        let file_type = entry.file_type().await.map_err(|error| {
            StorageError::new(format!(
                "Failed to inspect migration backup {}: {error}",
                entry.path().display()
            ))
        })?;
        if file_type.is_dir() {
            backups.push(entry.path());
        }
    }
    backups.sort();
    Ok(backups)
}

fn validate_migration_backup_dir(home: &Path, backup_dir: &Path) -> Result<(), StorageError> {
    let backup_root = std::fs::canonicalize(home.join("backups")).map_err(|error| {
        StorageError::new(format!(
            "Failed to resolve the migration backup root: {error}"
        ))
    })?;
    let resolved = std::fs::canonicalize(backup_dir).map_err(|error| {
        StorageError::new(format!(
            "Failed to resolve migration backup {}: {error}",
            backup_dir.display()
        ))
    })?;
    let valid_name = resolved
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("sqlite-migration-"));
    if resolved.parent() != Some(backup_root.as_path()) || !valid_name {
        return Err(StorageError::new(format!(
            "Migration journal backup path is outside the LingClaw backup directory: {}",
            backup_dir.display()
        )));
    }
    Ok(())
}

async fn move_legacy_dir(
    source: &Path,
    destination: &Path,
    expected: bool,
) -> Result<(), StorageError> {
    if source.exists() {
        if destination.exists() {
            return Err(StorageError::new(format!(
                "Both migration source {} and backup {} exist; refusing to guess",
                source.display(),
                destination.display()
            )));
        }
        tokio::fs::rename(source, destination)
            .await
            .map_err(|error| {
                StorageError::new(format!(
                    "Failed to move legacy storage {} to {}: {error}",
                    source.display(),
                    destination.display()
                ))
            })?;
    } else if expected && !destination.exists() {
        return Err(StorageError::new(format!(
            "Migration source {} and backup {} are both missing",
            source.display(),
            destination.display()
        )));
    }
    Ok(())
}

async fn restore_colliding_groups_session_workspace(
    home: &Path,
    backup_dir: &Path,
    manifest: &MigrationManifest,
) -> Result<(), StorageError> {
    let has_groups_session = manifest
        .files
        .iter()
        .any(|file| file.kind == "session" && ids_match(&file.id, "groups"));
    if !has_groups_session {
        return Ok(());
    }

    // `groups` was a valid Session id before SQLite storage and remains valid after
    // migration. Its workspace therefore shares the legacy Group JSON directory:
    // ~/.lingclaw/groups/workspace. Moving the legacy directory must not strand that
    // workspace in the migration backup.
    let source = backup_dir.join("groups").join("workspace");
    let destination = home.join("groups").join("workspace");
    match (source.exists(), destination.exists()) {
        (false, _) => Ok(()),
        (true, true) => Err(StorageError::new(format!(
            "Both the migrated groups Session workspace {} and destination {} exist; refusing to merge them",
            source.display(),
            destination.display()
        ))),
        (true, false) => {
            let parent = destination.parent().ok_or_else(|| {
                StorageError::new("The groups Session workspace has no parent directory")
            })?;
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                StorageError::new(format!(
                    "Failed to recreate groups Session directory {}: {error}",
                    parent.display()
                ))
            })?;
            tokio::fs::rename(&source, &destination)
                .await
                .map_err(|error| {
                    StorageError::new(format!(
                        "Failed to restore groups Session workspace {} to {}: {error}",
                        source.display(),
                        destination.display()
                    ))
                })
        }
    }
}

async fn import_backup(
    database: &Database,
    backup_dir: &Path,
    manifest: &MigrationManifest,
) -> Result<(), StorageError> {
    let sessions = load_legacy_sessions(&backup_dir.join("sessions"))?;
    let groups = load_legacy_groups(&backup_dir.join("groups"))?;
    validate_references(&sessions, &groups)?;
    validate_sqlite_import(&sessions, &groups)?;
    if sessions.len() != manifest.sessions || groups.len() != manifest.groups {
        return Err(StorageError::new(
            "Legacy migration backup no longer matches its manifest",
        ));
    }
    let expected_files = manifest
        .files
        .iter()
        .map(|file| {
            (
                file.kind.clone(),
                normalized_id(&file.id),
                file.sha256.clone(),
            )
        })
        .collect::<HashSet<_>>();
    let actual_files = sessions
        .iter()
        .map(|candidate| {
            (
                "session".to_string(),
                normalized_id(&candidate.id),
                candidate.sha256.clone(),
            )
        })
        .chain(groups.iter().map(|candidate| {
            (
                "group".to_string(),
                normalized_id(&candidate.id),
                candidate.sha256.clone(),
            )
        }))
        .collect::<HashSet<_>>();
    if expected_files != actual_files {
        return Err(StorageError::new(
            "Legacy migration backup files no longer match their recorded hashes",
        ));
    }
    let session_count = sessions.len();
    let group_count = groups.len();
    let sessions = sessions
        .into_iter()
        .map(|candidate| candidate.value)
        .collect::<Vec<_>>();
    let groups = groups
        .into_iter()
        .map(|candidate| candidate.value)
        .collect::<Vec<_>>();
    let metadata =
        serde_json::to_string(manifest).map_err(|error| StorageError::new(error.to_string()))?;
    database
        .call(move |connection| {
            let transaction = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            transaction.execute("DELETE FROM groups", [])?;
            transaction.execute("DELETE FROM sessions", [])?;
            for session in &sessions {
                super::session::save_session_record(&transaction, session)?;
            }
            for group in &groups {
                super::group::save_group_record(&transaction, group)?;
            }

            let persisted_sessions = transaction.query_row(
                "SELECT COUNT(*) FROM sessions",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            let persisted_groups = transaction.query_row(
                "SELECT COUNT(*) FROM groups",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            if persisted_sessions != i64::try_from(session_count).unwrap_or(i64::MAX)
                || persisted_groups != i64::try_from(group_count).unwrap_or(i64::MAX)
            {
                return Err(StorageError::new(format!(
                    "Legacy migration verification failed: expected {session_count}/{group_count} sessions/groups, found {persisted_sessions}/{persisted_groups}"
                )));
            }
            for session in &sessions {
                if !super::session::validate_session_record(&transaction, &session.id)? {
                    return Err(StorageError::new(format!(
                        "Legacy migration verification failed for session '{}'",
                        session.id
                    )));
                }
            }
            for group in &groups {
                if !super::group::validate_group_record(&transaction, &group.id)? {
                    return Err(StorageError::new(format!(
                        "Legacy migration verification failed for group '{}'",
                        group.id
                    )));
                }
            }
            transaction.execute(
                "INSERT INTO storage_metadata(key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                rusqlite::params![MIGRATION_METADATA_KEY, metadata],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
}

pub(crate) async fn migrate_legacy_json_if_needed(
    database: &Database,
) -> Result<Option<PathBuf>, StorageError> {
    let home = database
        .path()
        .parent()
        .ok_or_else(|| StorageError::new("SQLite database has no parent directory"))?
        .to_path_buf();
    let sessions_dir = home.join("sessions");
    let groups_dir = home.join("groups");
    let journal_path = home.join(MIGRATION_JOURNAL_FILE);
    if database.metadata(MIGRATION_METADATA_KEY).await?.is_some() {
        cleanup_migration_journal_artifacts(&journal_path).await?;
        return Ok(None);
    }

    let mut journal = if let Some(journal) = recover_migration_journal(&home, &journal_path).await?
    {
        journal
    } else {
        let sessions = load_legacy_sessions(&sessions_dir)?;
        let groups = load_legacy_groups(&groups_dir)?;
        validate_references(&sessions, &groups)?;
        validate_sqlite_import(&sessions, &groups)?;
        let had_sessions_dir = sessions_dir.exists();
        let had_groups_dir = groups_dir.exists();
        if !had_sessions_dir && !had_groups_dir {
            let stranded_backups = find_migration_backup_dirs(&home).await?;
            if !stranded_backups.is_empty() {
                let backup_paths = stranded_backups
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(StorageError::new(format!(
                    "Found legacy SQLite migration backup(s) without a recoverable migration \
                     journal: {backup_paths}. Refusing to mark migration complete; restore a \
                     migration journal or inspect the backup before retrying"
                )));
            }
            let counts = database.entity_counts().await?;
            let manifest = MigrationManifest {
                version: 1,
                created_at: crate::now_epoch(),
                sessions: counts.0 as usize,
                groups: counts.1 as usize,
                files: Vec::new(),
            };
            database
                .set_metadata(
                    MIGRATION_METADATA_KEY,
                    &serde_json::to_string(&manifest)
                        .map_err(|error| StorageError::new(error.to_string()))?,
                )
                .await?;
            return Ok(None);
        }
        if database.entity_counts().await? != (0, 0) {
            return Err(StorageError::new(
                "SQLite contains data while unmigrated JSON storage also exists; refusing to merge",
            ));
        }
        let backup_dir = next_backup_dir(&home);
        tokio::fs::create_dir_all(&backup_dir)
            .await
            .map_err(|error| {
                StorageError::new(format!(
                    "Failed to create migration backup {}: {error}",
                    backup_dir.display()
                ))
            })?;
        let manifest = manifest_for(&home, &sessions, &groups);
        let journal = MigrationJournal {
            version: 1,
            phase: "prepared".to_string(),
            backup_dir,
            had_sessions_dir,
            had_groups_dir,
            manifest,
        };
        write_json_file(&journal_path, &journal).await?;
        journal
    };

    validate_migration_journal(&home, &journal_path, &journal)?;
    if journal.phase == "prepared" {
        move_legacy_dir(
            &sessions_dir,
            &journal.backup_dir.join("sessions"),
            journal.had_sessions_dir,
        )
        .await?;
        move_legacy_dir(
            &groups_dir,
            &journal.backup_dir.join("groups"),
            journal.had_groups_dir,
        )
        .await?;
        journal.phase = "moved".to_string();
        write_json_file(&journal_path, &journal).await?;
    }

    restore_colliding_groups_session_workspace(&home, &journal.backup_dir, &journal.manifest)
        .await?;
    write_json_file(
        &journal.backup_dir.join("migration-manifest.json"),
        &journal.manifest,
    )
    .await?;
    import_backup(database, &journal.backup_dir, &journal.manifest).await?;
    cleanup_migration_journal_artifacts(&journal_path).await?;
    Ok(Some(journal.backup_dir))
}
