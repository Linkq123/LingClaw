use serde::Serialize;
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(test)]
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::SystemTime,
};

use crate::{
    Config, Session, SessionWorkspaceKind, config_dir_path, context_input_budget_for_model,
    estimate_tokens_for_provider, format_token_count, format_usage_block, prompts,
};

use super::{AppState, ChatMessage};

pub(crate) use crate::storage::SessionModelPreferences;

#[derive(Clone)]
#[cfg(test)]
struct PersistedSessionCacheEntry {
    payload_hash: u64,
    payload_len: usize,
    file_mtime: Option<SystemTime>,
    file_len: u64,
}

#[derive(Clone)]
pub(crate) struct SessionSummary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) model_override: Option<String>,
    pub(crate) messages: usize,
    pub(crate) tool_calls: usize,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) corrupt: bool,
    pub(crate) workspace_kind: SessionWorkspaceKind,
    pub(crate) working_directory: PathBuf,
}

impl SessionSummary {
    pub(crate) fn from_session(session: &Session) -> Self {
        Self {
            id: session.id.clone(),
            name: session.name.clone(),
            model_override: session.model_override.clone(),
            messages: sanitized_non_system_message_count(session),
            tool_calls: session.tool_calls_count,
            created_at: session.created_at,
            updated_at: session.updated_at,
            corrupt: false,
            workspace_kind: session.workspace_kind,
            working_directory: session.working_directory.clone(),
        }
    }

    pub(crate) fn to_json(
        &self,
        config: &Config,
        session: Option<&Session>,
        workspace_available: bool,
    ) -> serde_json::Value {
        let model = session
            .map(|session| session.effective_model(&config.model).to_string())
            .or_else(|| self.model_override.clone())
            .unwrap_or_else(|| config.model.clone());
        // `std::fs::canonicalize` returns extended-length `\\?\` paths on
        // Windows. Keep those internally for stable matching, but never expose
        // the implementation prefix in the WebUI/TUI-facing summary.
        let workspace_path = crate::display_working_directory(&self.working_directory);
        let display_name = self
            .working_directory
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(&workspace_path)
            .to_string();
        json!({
            "id": self.id,
            "name": self.name,
            "messages": self.messages,
            "tool_calls": self.tool_calls,
            "model": model,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "corrupt": self.corrupt,
            "workspace": {
                "kind": self.workspace_kind.as_str(),
                "path": workspace_path,
                "display_name": display_name,
                "available": workspace_available,
            },
        })
    }
}

/// Probe a working directory without blocking a Tokio worker or allowing a
/// disconnected network/removable path to stall Session discovery indefinitely.
pub(crate) async fn working_directory_available(path: &Path) -> bool {
    tokio::time::timeout(
        std::time::Duration::from_millis(750),
        tokio::fs::metadata(path),
    )
    .await
    .is_ok_and(|result| result.is_ok_and(|metadata| metadata.is_dir()))
}

#[cfg(test)]
type PersistedSessionCacheLock = OnceLock<Mutex<HashMap<String, PersistedSessionCacheEntry>>>;
#[cfg(test)]
static PERSISTED_SESSION_CACHE: PersistedSessionCacheLock = OnceLock::new();
static SESSION_SAVE_WRITES: AtomicU64 = AtomicU64::new(0);
static SESSION_SAVE_SKIPS: AtomicU64 = AtomicU64::new(0);
type SessionPersistGateLock = OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;
static SESSION_PERSIST_GATES: SessionPersistGateLock = OnceLock::new();

#[cfg(test)]
fn session_persist_cache() -> &'static Mutex<HashMap<String, PersistedSessionCacheEntry>> {
    PERSISTED_SESSION_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_persist_gates() -> &'static Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    SESSION_PERSIST_GATES.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn session_persist_gate(session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let gate_key = if cfg!(windows) {
        session_id.to_ascii_lowercase()
    } else {
        session_id.to_string()
    };
    let mut guard = session_persist_gates()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard
        .entry(gate_key)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

#[cfg(test)]
fn session_payload_hash(data: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
async fn session_file_signature(path: &Path) -> Option<(Option<SystemTime>, u64)> {
    let metadata = tokio::fs::metadata(path).await.ok()?;
    Some((metadata.modified().ok(), metadata.len()))
}

#[cfg(test)]
fn should_skip_session_write(
    session_id: &str,
    payload_hash: u64,
    payload_len: usize,
    file_signature: Option<(Option<SystemTime>, u64)>,
) -> bool {
    let Some((file_mtime, file_len)) = file_signature else {
        return false;
    };
    let guard = session_persist_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard.get(session_id).is_some_and(|entry| {
        entry.payload_hash == payload_hash
            && entry.payload_len == payload_len
            && entry.file_mtime == file_mtime
            && entry.file_len == file_len
    })
}

#[cfg(test)]
fn update_session_persist_cache(
    session_id: &str,
    payload_hash: u64,
    payload_len: usize,
    file_signature: Option<(Option<SystemTime>, u64)>,
) {
    let Some((file_mtime, file_len)) = file_signature else {
        return;
    };
    let mut guard = session_persist_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard.insert(
        session_id.to_string(),
        PersistedSessionCacheEntry {
            payload_hash,
            payload_len,
            file_mtime,
            file_len,
        },
    );
}

#[derive(Serialize)]
struct PersistedSessionView<'a> {
    id: &'a str,
    name: &'a str,
    messages: Vec<ChatMessage>,
    created_at: u64,
    updated_at: u64,
    tool_calls_count: usize,
    input_tokens: u64,
    output_tokens: u64,
    daily_input_tokens: u64,
    daily_output_tokens: u64,
    input_token_source: &'a str,
    output_token_source: &'a str,
    token_usage_day: &'a str,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    daily_provider_usage: &'a HashMap<String, [u64; 2]>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    total_label_usage: &'a HashMap<String, [u64; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    usage_history: &'a Vec<crate::DailyUsageSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_override: Option<&'a str>,
    think_level: &'a str,
    show_react: bool,
    show_tools: bool,
    show_reasoning: bool,
    #[serde(skip_serializing_if = "HashSet::is_empty")]
    enabled_system_skills: &'a HashSet<String>,
    #[serde(skip_serializing_if = "HashSet::is_empty")]
    failed_tool_results: HashSet<String>,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    subagent_snapshots: HashMap<String, crate::SubagentHistorySnapshot>,
    todos: &'a crate::todos::TodoSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_plan: Option<&'a crate::PendingPlan>,
    version: u32,
}

pub(crate) fn session_persist_metrics() -> (u64, u64) {
    (
        SESSION_SAVE_WRITES.load(Ordering::Relaxed),
        SESSION_SAVE_SKIPS.load(Ordering::Relaxed),
    )
}

const RESERVED_SESSION_IDS: &[&str] = &[
    "agents",
    "backups",
    "lingclaw.db",
    "lingclaw.db-journal",
    "lingclaw.db-shm",
    "lingclaw.db-wal",
    "memory",
    "sessions",
    "skills",
    "sqlite-migration.json",
    "sqlite-migration.json.lingclaw-save-backup",
    "sqlite-migration.json.recovery-backup",
    "sqlite-migration.json.recovery.tmp",
    "sqlite-migration.json.tmp",
    "static",
    "system-agents",
    "system-skills",
];

const STORAGE_OWNED_TOP_LEVEL_PATHS: &[&str] = &[
    "backups",
    "lingclaw.db",
    "lingclaw.db-journal",
    "lingclaw.db-shm",
    "lingclaw.db-wal",
    "sqlite-migration.json",
    "sqlite-migration.json.lingclaw-save-backup",
    "sqlite-migration.json.recovery-backup",
    "sqlite-migration.json.recovery.tmp",
    "sqlite-migration.json.tmp",
];

pub(crate) fn is_storage_owned_session_id(id: &str) -> bool {
    STORAGE_OWNED_TOP_LEVEL_PATHS
        .iter()
        .any(|name| id.eq_ignore_ascii_case(name))
}

const WINDOWS_RESERVED_DEVICE_NAMES: &[&str] = &[
    "aux", "con", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1",
    "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9", "nul", "prn",
];

pub(crate) fn validate_session_id(id: &str) -> Result<&str, String> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err("Session id cannot be empty.".to_string());
    }
    if trimmed == "." || trimmed == ".." || trimmed.ends_with('.') {
        return Err("Invalid session id.".to_string());
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(
            "Invalid session id. Use only letters, numbers, dots, dashes, or underscores."
                .to_string(),
        );
    }
    // Canonicalize any case-variant of the main session id ("Main", "MAIN", ...)
    // to the literal "main". Without this, a case-variant id is creatable via /ws
    // and becomes a DISTINCT session on a case-sensitive filesystem, which every
    // case-insensitive main guard then permanently over-rejects from dispatch/group.
    if crate::is_main(trimmed) {
        return Ok(crate::MAIN_SESSION_ID);
    }
    let lowered = trimmed.to_ascii_lowercase();
    if RESERVED_SESSION_IDS.contains(&lowered.as_str()) {
        return Err("Invalid session id. This name is reserved.".to_string());
    }
    let windows_reserved_name = lowered.split('.').next().unwrap_or("");
    if WINDOWS_RESERVED_DEVICE_NAMES.contains(&windows_reserved_name) {
        return Err("Invalid session id. This name is reserved on Windows.".to_string());
    }
    Ok(trimmed)
}

pub(crate) fn session_workspace_root_for_delete(session_id: &str) -> Result<PathBuf, String> {
    let session_id = validate_session_id(session_id)?;
    let home = crate::config_dir_path().unwrap_or_else(|| PathBuf::from(".lingclaw"));
    let workspace_root = crate::session_workspace_path(session_id)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "Session workspace has no parent directory.".to_string())?;
    if workspace_root.parent() != Some(home.as_path()) {
        return Err("Refusing to delete a session workspace outside LingClaw home.".to_string());
    }

    let is_storage_owned = STORAGE_OWNED_TOP_LEVEL_PATHS.iter().any(|name| {
        let protected = home.join(name);
        if cfg!(windows) {
            protected
                .to_string_lossy()
                .eq_ignore_ascii_case(&workspace_root.to_string_lossy())
        } else {
            protected == workspace_root
        }
    });
    if is_storage_owned {
        return Err("Refusing to delete a LingClaw storage-owned path.".to_string());
    }
    Ok(workspace_root)
}

pub(crate) fn subagent_snapshot_storage_key(tool_call_id: &str, occurrence: usize) -> String {
    format!("{tool_call_id}@{occurrence}")
}

fn next_tool_occurrence(counts: &mut HashMap<String, usize>, tool_call_id: &str) -> usize {
    let count = counts.entry(tool_call_id.to_string()).or_insert(0);
    *count += 1;
    *count
}

fn canonicalize_subagent_snapshots_for_messages(
    messages: &[ChatMessage],
    snapshots: &HashMap<String, crate::SubagentHistorySnapshot>,
) -> HashMap<String, crate::SubagentHistorySnapshot> {
    let mut totals: HashMap<String, usize> = HashMap::new();
    for message in messages.iter().filter(|message| message.role == "tool") {
        if let Some(tool_call_id) = message
            .tool_call_id
            .as_deref()
            .filter(|tool_call_id| !tool_call_id.is_empty())
        {
            *totals.entry(tool_call_id.to_string()).or_insert(0) += 1;
        }
    }

    let mut occurrences: HashMap<String, usize> = HashMap::new();
    let mut out = HashMap::new();
    for message in messages.iter().filter(|message| message.role == "tool") {
        let Some(tool_call_id) = message
            .tool_call_id
            .as_deref()
            .filter(|tool_call_id| !tool_call_id.is_empty())
        else {
            continue;
        };
        let occurrence = next_tool_occurrence(&mut occurrences, tool_call_id);
        let key = subagent_snapshot_storage_key(tool_call_id, occurrence);
        if let Some(snapshot) = snapshots.get(&key).cloned() {
            out.insert(key, snapshot);
            continue;
        }
        if occurrence == totals.get(tool_call_id).copied().unwrap_or(0)
            && let Some(snapshot) = snapshots.get(tool_call_id).cloned()
        {
            out.insert(key, snapshot);
        }
    }

    out
}

fn tool_messages_match(old_message: &ChatMessage, new_message: &ChatMessage) -> bool {
    old_message.role == "tool"
        && new_message.role == "tool"
        && old_message.tool_call_id == new_message.tool_call_id
        && old_message.content == new_message.content
        && old_message.timestamp == new_message.timestamp
}

fn normalize_subagent_snapshots_for_messages(
    messages: &[ChatMessage],
    snapshots: &HashMap<String, crate::SubagentHistorySnapshot>,
) -> HashMap<String, crate::SubagentHistorySnapshot> {
    canonicalize_subagent_snapshots_for_messages(messages, snapshots)
}

pub(crate) fn remap_subagent_snapshots_for_message_rewrite(
    old_messages: &[ChatMessage],
    new_messages: &[ChatMessage],
    old_snapshots: &HashMap<String, crate::SubagentHistorySnapshot>,
) -> HashMap<String, crate::SubagentHistorySnapshot> {
    let canonical_old = canonicalize_subagent_snapshots_for_messages(old_messages, old_snapshots);

    let mut old_occurrences: HashMap<String, usize> = HashMap::new();
    let mut old_tool_entries: Vec<(&ChatMessage, Option<crate::SubagentHistorySnapshot>)> =
        Vec::new();
    for message in old_messages.iter().filter(|message| message.role == "tool") {
        let snapshot = message
            .tool_call_id
            .as_deref()
            .filter(|tool_call_id| !tool_call_id.is_empty())
            .and_then(|tool_call_id| {
                let occurrence = next_tool_occurrence(&mut old_occurrences, tool_call_id);
                canonical_old
                    .get(&subagent_snapshot_storage_key(tool_call_id, occurrence))
                    .cloned()
            });
        old_tool_entries.push((message, snapshot));
    }

    let mut new_occurrences: HashMap<String, usize> = HashMap::new();
    let mut out = HashMap::new();
    let mut cursor = 0usize;

    for message in new_messages.iter().filter(|message| message.role == "tool") {
        while cursor < old_tool_entries.len()
            && !tool_messages_match(old_tool_entries[cursor].0, message)
        {
            cursor += 1;
        }
        let matched_snapshot = if cursor < old_tool_entries.len() {
            let snapshot = old_tool_entries[cursor].1.clone();
            cursor += 1;
            snapshot
        } else {
            None
        };

        let Some(tool_call_id) = message
            .tool_call_id
            .as_deref()
            .filter(|tool_call_id| !tool_call_id.is_empty())
        else {
            continue;
        };
        let occurrence = next_tool_occurrence(&mut new_occurrences, tool_call_id);
        if let Some(snapshot) = matched_snapshot {
            out.insert(
                subagent_snapshot_storage_key(tool_call_id, occurrence),
                snapshot,
            );
        }
    }

    out
}

pub(crate) fn normalize_subagent_snapshots(session: &mut Session) {
    session.subagent_snapshots =
        normalize_subagent_snapshots_for_messages(&session.messages, &session.subagent_snapshots);
}

fn remap_plan_indices_for_message_rewrite(
    session: &mut Session,
    old_messages: &[ChatMessage],
    new_messages: &[ChatMessage],
) {
    let Some(plan) = session.pending_plan.as_mut() else {
        return;
    };
    let old = old_messages
        .iter()
        .map(serde_json::to_vec)
        .collect::<Result<Vec<_>, _>>();
    let new = new_messages
        .iter()
        .map(serde_json::to_vec)
        .collect::<Result<Vec<_>, _>>();
    let (Ok(old), Ok(new)) = (old, new) else {
        plan.original_user_message_index = 0;
        plan.assistant_plan_message_index = 0;
        return;
    };
    let retained = (1..old.len()).find_map(|old_start| {
        let suffix_len = old.len() - old_start;
        if suffix_len > new.len().saturating_sub(1) {
            return None;
        }
        (1..=new.len() - suffix_len)
            .find(|&new_start| old[old_start..] == new[new_start..new_start + suffix_len])
            .map(|new_start| (old_start, new_start))
    });
    let (old_start, new_start) = retained.unwrap_or((old.len(), new.len()));
    let remap = |index: usize| {
        if index >= old_start {
            new_start.saturating_add(index - old_start)
        } else {
            0
        }
    };
    plan.original_user_message_index = remap(plan.original_user_message_index);
    plan.assistant_plan_message_index = remap(plan.assistant_plan_message_index);
}

pub(crate) fn replace_session_messages(session: &mut Session, new_messages: Vec<ChatMessage>) {
    let old_messages = session.messages.clone();
    session.subagent_snapshots = remap_subagent_snapshots_for_message_rewrite(
        &old_messages,
        &new_messages,
        &session.subagent_snapshots,
    );
    remap_plan_indices_for_message_rewrite(session, &old_messages, &new_messages);
    session.messages = new_messages;
    retain_failed_tool_results(session);
}

pub(crate) fn sessions_dir() -> PathBuf {
    let dir = config_dir_path()
        .unwrap_or_else(|| PathBuf::from(".lingclaw"))
        .join("sessions");
    #[cfg(test)]
    std::fs::create_dir_all(&dir).ok();
    dir
}

pub(crate) fn replace_session_file_from_temp(path: &Path, tmp_path: &Path) -> Result<(), String> {
    let backup_path = path.with_extension("json.lingclaw-save-backup");
    match std::fs::remove_file(&backup_path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.to_string()),
    }

    match std::fs::rename(tmp_path, path) {
        Ok(()) => return Ok(()),
        Err(first_err) => {
            if !path.exists() {
                return Err(first_err.to_string());
            }
        }
    }

    std::fs::rename(path, &backup_path).map_err(|e| e.to_string())?;

    match std::fs::rename(tmp_path, path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&backup_path);
            Ok(())
        }
        Err(final_err) => {
            let _ = std::fs::rename(&backup_path, path);
            Err(final_err.to_string())
        }
    }
}

async fn save_session_to_disk_inner(session: &Session) -> Result<(), String> {
    #[cfg(not(test))]
    {
        crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .save_session(session)
            .await
            .map_err(|error| error.to_string())?;
        SESSION_SAVE_WRITES.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    #[cfg(test)]
    {
        let path = sessions_dir().join(format!("{}.json", session.id));
        let tmp_path = sessions_dir().join(format!("{}.json.tmp", session.id));
        let data = build_session_persist_payload(session)?;
        let payload_hash = session_payload_hash(&data);
        let payload_len = data.len();
        if should_skip_session_write(
            &session.id,
            payload_hash,
            payload_len,
            session_file_signature(&path).await,
        ) {
            SESSION_SAVE_SKIPS.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        tokio::fs::write(&tmp_path, data)
            .await
            .map_err(|e| e.to_string())?;

        if let Err(e) = replace_session_file_from_temp(&path, &tmp_path) {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(e);
        }
        SESSION_SAVE_WRITES.fetch_add(1, Ordering::Relaxed);
        update_session_persist_cache(
            &session.id,
            payload_hash,
            payload_len,
            session_file_signature(&path).await,
        );
        Ok(())
    }
}

async fn reset_session_context_on_disk_inner(session: &Session) -> Result<(), String> {
    #[cfg(not(test))]
    {
        crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .reset_session_context(session)
            .await
            .map_err(|error| error.to_string())?;
        SESSION_SAVE_WRITES.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    #[cfg(test)]
    {
        // Unit tests retain the legacy JSON persistence shim. A reset writes
        // the already-cleared Session snapshot, which contains no plan history.
        save_session_to_disk_inner(session).await
    }
}

pub(crate) async fn save_session_to_disk_locked(session: &Session) -> Result<(), String> {
    save_session_to_disk_inner(session).await
}

pub(crate) fn load_session_model_preferences_result(
    id: &str,
) -> Result<Option<SessionModelPreferences>, String> {
    #[cfg(not(test))]
    {
        crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .load_session_model_preferences_blocking(id)
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    {
        Ok(
            load_session_from_storage_result(id)?.map(|session| SessionModelPreferences {
                id: session.id,
                model_override: session.model_override,
                think_level: session.think_level,
                updated_at: session.updated_at,
            }),
        )
    }
}

/// Persist only the model-effort portion of a Session while its persist gate
/// is held. Config hot reload uses this path for every Session so it does not
/// rewrite histories or create workspaces merely to normalize effort.
pub(crate) async fn update_session_think_level_locked(
    preferences: &SessionModelPreferences,
) -> Result<bool, String> {
    #[cfg(not(test))]
    {
        crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .update_session_think_level(
                &preferences.id,
                &preferences.think_level,
                preferences.updated_at,
            )
            .await
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    {
        let Some(mut session) = load_session_from_storage_result(&preferences.id)? else {
            return Ok(false);
        };
        session.think_level = preferences.think_level.clone();
        session.updated_at = session.updated_at.max(preferences.updated_at);
        save_session_to_disk_inner(&session).await?;
        Ok(true)
    }
}

pub(crate) async fn reset_session_context_on_disk_locked(session: &Session) -> Result<(), String> {
    reset_session_context_on_disk_inner(session).await
}

#[cfg(test)]
pub(crate) async fn save_session_to_disk(session: &Session) -> Result<(), String> {
    let persist_gate = session_persist_gate(&session.id);
    let _persist_guard = persist_gate.lock().await;
    save_session_to_disk_inner(session).await
}

pub(crate) async fn save_current_session_to_disk(
    state: &AppState,
    session_id: &str,
) -> Result<(), String> {
    let persist_gate = session_persist_gate(session_id);
    let _persist_guard = persist_gate.lock().await;
    let snapshot = {
        let sessions = state.sessions.lock().await;
        sessions.get(session_id).cloned()
    };
    let session = snapshot.ok_or_else(|| "Session not found".to_string())?;
    save_session_to_disk_inner(&session).await
}

pub(crate) async fn delete_session_from_storage(
    session_id: &str,
) -> Result<crate::storage::SessionDeleteOutcome, String> {
    let session_id = validate_session_id(session_id)?.to_string();
    #[cfg(not(test))]
    {
        crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .delete_session(&session_id)
            .await
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    {
        let mut deleted = false;
        for path in [
            sessions_dir().join(format!("{session_id}.json")),
            sessions_dir().join(format!("{session_id}.json.tmp")),
        ] {
            match tokio::fs::remove_file(path).await {
                Ok(()) => deleted = true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(crate::storage::SessionDeleteOutcome {
            deleted,
            affected_group_ids: Vec::new(),
        })
    }
}

pub(crate) fn sanitize_session_messages(messages: &mut Vec<ChatMessage>) {
    messages.retain(|message| !message.is_empty_assistant_message());
}

pub(crate) fn trim_incomplete_tool_calls(messages: &mut Vec<ChatMessage>) {
    let ast_idx = messages.iter().rposition(|m| {
        m.role == "assistant" && m.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
    });
    let Some(idx) = ast_idx else {
        sanitize_session_messages(messages);
        return;
    };
    let expected = messages[idx]
        .tool_calls
        .as_ref()
        .map(|tc| tc.len())
        .unwrap_or(0);
    let actual = messages[idx + 1..]
        .iter()
        .filter(|m| m.role == "tool")
        .count();
    if actual < expected {
        let removed = messages.len() - idx;
        eprintln!(
            "trim_incomplete_tool_calls: removed {removed} trailing messages (expected {expected} tool results, found {actual})"
        );
        messages.truncate(idx);
    }

    sanitize_session_messages(messages);
}

pub(crate) fn trim_incomplete_tool_calls_in_session(session: &mut Session) {
    let old_messages = session.messages.clone();
    trim_incomplete_tool_calls(&mut session.messages);
    session.subagent_snapshots = remap_subagent_snapshots_for_message_rewrite(
        &old_messages,
        &session.messages,
        &session.subagent_snapshots,
    );
    retain_failed_tool_results(session);
}

pub(crate) fn normalize_session(session: &mut Session) {
    super::migrate_session(session);
    trim_incomplete_tool_calls_in_session(session);
    sanitize_exec_tool_args_in_session(session);
    normalize_subagent_snapshots(session);
    retain_failed_tool_results(session);
}

fn retain_failed_tool_results(session: &mut Session) {
    retain_failed_tool_results_for_messages(&session.messages, &mut session.failed_tool_results);
}

fn retain_failed_tool_results_for_messages(
    messages: &[ChatMessage],
    failed_tool_results: &mut HashSet<String>,
) {
    let valid_ids: HashSet<&str> = messages
        .iter()
        .filter(|message| message.role == "tool")
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect();
    failed_tool_results.retain(|tool_id| valid_ids.contains(tool_id.as_str()));
}

fn build_session_persist_payload(session: &Session) -> Result<String, String> {
    let mut messages = session.messages.clone();
    sanitize_session_messages(&mut messages);
    for message in &mut messages {
        crate::tools::sanitize_chat_message_tool_calls_in_place(message);
        if message.role == "tool"
            && let Some(images) = message.images.as_mut()
        {
            for image in images {
                if image.s3_object_key.is_some() {
                    // Signed URLs are request-scoped. Persist the stable object
                    // identity and regenerate a URL on each restore/request.
                    image.url.clear();
                }
            }
        }
    }
    let subagent_snapshots =
        normalize_subagent_snapshots_for_messages(&messages, &session.subagent_snapshots)
            .into_iter()
            .map(|(key, mut snapshot)| {
                crate::tools::sanitize_subagent_snapshot_tool_args_in_place(&mut snapshot);
                for tool in &mut snapshot.tools {
                    for image in &mut tool.images {
                        if image.s3_object_key.is_some() {
                            image.url.clear();
                        }
                    }
                }
                (key, snapshot)
            })
            .collect();
    let mut failed_tool_results = session.failed_tool_results.clone();
    retain_failed_tool_results_for_messages(&messages, &mut failed_tool_results);

    serde_json::to_string(&PersistedSessionView {
        id: &session.id,
        name: &session.name,
        messages,
        created_at: session.created_at,
        updated_at: session.updated_at,
        tool_calls_count: session.tool_calls_count,
        input_tokens: session.input_tokens,
        output_tokens: session.output_tokens,
        daily_input_tokens: session.daily_input_tokens,
        daily_output_tokens: session.daily_output_tokens,
        input_token_source: &session.input_token_source,
        output_token_source: &session.output_token_source,
        token_usage_day: &session.token_usage_day,
        daily_provider_usage: &session.daily_provider_usage,
        total_label_usage: &session.total_label_usage,
        usage_history: &session.usage_history,
        model_override: session.model_override.as_deref(),
        think_level: &session.think_level,
        show_react: session.show_react,
        show_tools: session.show_tools,
        show_reasoning: session.show_reasoning,
        enabled_system_skills: &session.enabled_system_skills,
        failed_tool_results,
        subagent_snapshots,
        todos: &session.todos,
        pending_plan: session.pending_plan.as_ref(),
        version: session.version,
    })
    .map_err(|e| e.to_string())
}

pub(crate) fn session_for_storage(session: &Session) -> Result<Session, String> {
    let payload = build_session_persist_payload(session)?;
    let mut stored: Session = serde_json::from_str(&payload).map_err(|error| error.to_string())?;
    normalize_session(&mut stored);
    stored.workspace = crate::session_workspace_path(&stored.id);
    stored.working_directory = session.working_directory.clone();
    stored.workspace_kind = session.workspace_kind;
    Ok(stored)
}

#[cfg(test)]
pub(crate) fn load_session_snapshot_from_path(path: &Path) -> Option<Session> {
    let data = std::fs::read_to_string(path).ok()?;
    let mut session: Session = serde_json::from_str(&data).ok()?;
    normalize_session(&mut session);
    // Legacy JSON snapshots never carried workspace metadata. Test-mode JSON
    // storage therefore has the same v5 migration semantics as production:
    // every recovered Session is managed and rooted in its private home.
    session.workspace = crate::session_workspace_path(&session.id);
    session.working_directory = session.workspace.clone();
    session.workspace_kind = SessionWorkspaceKind::Managed;
    Some(session)
}

pub(crate) fn canonical_saved_session_id_result(id: &str) -> Result<Option<String>, String> {
    #[cfg(not(test))]
    {
        let id = validate_session_id(id)?;
        crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .canonical_session_id_blocking(id)
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    {
        let id = match validate_session_id(id) {
            Ok(id) => id,
            Err(_) => return Ok(None),
        };
        let dir = sessions_dir();
        let mut fallback = None;
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                if stem == id {
                    return Ok(Some(stem.to_string()));
                }
                if cfg!(windows) && stem.eq_ignore_ascii_case(id) {
                    fallback = Some(stem.to_string());
                }
            }
        }
        Ok(fallback)
    }
}

#[cfg(test)]
pub(crate) fn canonical_saved_session_id(id: &str) -> Option<String> {
    canonical_saved_session_id_result(id).ok().flatten()
}

pub(crate) fn load_session_from_storage_result(id: &str) -> Result<Option<Session>, String> {
    #[cfg(not(test))]
    {
        let Some(id) = canonical_saved_session_id_result(id)? else {
            return Ok(None);
        };
        let session = crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .load_session_blocking(&id)
            .map_err(|error| error.to_string())?;
        let Some(mut session) = session else {
            return Ok(None);
        };
        session.workspace = super::session_workspace_path(&session.id);
        std::fs::create_dir_all(&session.workspace).ok();
        prompts::ensure_session_workspace(&session.workspace);
        if session.workspace_kind == SessionWorkspaceKind::Managed {
            session.working_directory = session.workspace.clone();
        }
        Ok(Some(session))
    }

    #[cfg(test)]
    {
        let Some(id) = canonical_saved_session_id(id)
            .or_else(|| validate_session_id(id).ok().map(str::to_string))
        else {
            return Ok(None);
        };
        let path = sessions_dir().join(format!("{id}.json"));
        let tmp_path = sessions_dir().join(format!("{id}.json.tmp"));
        // Load from primary, fall back to .tmp, or pick the newer of the two.
        // Crash scenarios: (a) primary missing, tmp exists → use tmp;
        // (b) both exist, tmp is newer → use tmp (crash after tmp write, before rename);
        // (c) both exist, primary is newer → use primary (normal case).
        let primary = load_session_snapshot_from_path(&path);
        let tmp_available = tmp_path.exists();
        let mut session = match (primary, tmp_available) {
            (Some(p), false) => p,
            (None, true) => {
                eprintln!(
                    "Warning: recovering session '{id}' from .tmp file (primary missing after crash)"
                );
                let Some(session) = load_session_snapshot_from_path(&tmp_path) else {
                    return Ok(None);
                };
                session
            }
            (Some(p), true) => {
                // Both exist — pick the one with the later mtime.
                let primary_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                let tmp_mtime = std::fs::metadata(&tmp_path).and_then(|m| m.modified()).ok();
                if tmp_mtime >= primary_mtime {
                    eprintln!(
                        "Warning: recovering session '{id}' from newer .tmp file (crash during save)"
                    );
                    load_session_snapshot_from_path(&tmp_path).unwrap_or(p)
                } else {
                    // tmp is stale leftover — clean it up.
                    let _ = std::fs::remove_file(&tmp_path);
                    p
                }
            }
            (None, false) => return Ok(None),
        };
        session.workspace = super::session_workspace_path(&session.id);
        std::fs::create_dir_all(&session.workspace).ok();
        prompts::ensure_session_workspace(&session.workspace);
        session.workspace_kind = SessionWorkspaceKind::Managed;
        session.working_directory = session.workspace.clone();
        Ok(Some(session))
    }
}

#[cfg(test)]
pub(crate) fn load_session_from_disk(id: &str) -> Option<Session> {
    load_session_from_storage_result(id).ok().flatten()
}

pub(crate) async fn build_refreshed_session_system_prompt(
    state: &AppState,
    session: &Session,
) -> crate::ChatMessage {
    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    crate::prompts::build_system_prompt_for_working_directory_async(
        &config,
        &session.workspace,
        &session.working_directory,
        &model,
        &session.enabled_system_skills,
    )
    .await
}

pub(crate) fn replace_session_system_prompt(session: &mut Session, sys: crate::ChatMessage) {
    if let Some(first) = session.messages.first_mut()
        && first.role == "system"
    {
        *first = sys;
    }
}

pub(crate) fn sanitized_non_system_message_count(session: &Session) -> usize {
    let mut normalized = session.clone();
    normalize_session(&mut normalized);
    normalized
        .messages
        .iter()
        .filter(|message| message.role != "system")
        .count()
}

pub(crate) fn list_saved_session_summaries_result(
    dir: &Path,
) -> Result<Vec<SessionSummary>, String> {
    list_saved_session_summaries_for_workspace_result(dir, None)
}

pub(crate) fn list_saved_session_summaries_for_workspace_result(
    dir: &Path,
    working_directory_key: Option<&str>,
) -> Result<Vec<SessionSummary>, String> {
    #[cfg(not(test))]
    {
        let _ = dir;
        let database = crate::storage::Database::global().map_err(|error| error.to_string())?;
        match working_directory_key {
            Some(key) => database
                .list_session_summaries_for_working_directory_blocking(key)
                .map_err(|error| error.to_string()),
            None => database
                .list_session_summaries_blocking()
                .map_err(|error| error.to_string()),
        }
    }

    #[cfg(test)]
    {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json") {
                    if let Some(session) = load_session_snapshot_from_path(&path) {
                        out.push(SessionSummary::from_session(&session));
                    } else if let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) {
                        out.push(SessionSummary {
                            id: id.to_string(),
                            name: "[Corrupt Session]".to_string(),
                            model_override: None,
                            messages: 0,
                            tool_calls: 0,
                            created_at: 0,
                            updated_at: 0,
                            corrupt: true,
                            workspace_kind: SessionWorkspaceKind::Managed,
                            working_directory: crate::session_workspace_path(id),
                        });
                    }
                }
            }
        }
        if let Some(expected_key) = working_directory_key {
            out.retain(|summary| {
                crate::working_directory_key(&summary.working_directory)
                    .ok()
                    .as_deref()
                    == Some(expected_key)
            });
        }
        sort_session_summaries(&mut out);
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) fn list_saved_session_summaries_in_dir(dir: &Path) -> Vec<SessionSummary> {
    list_saved_session_summaries_result(dir).unwrap_or_default()
}

pub(crate) fn sort_session_summaries(summaries: &mut [SessionSummary]) {
    summaries.sort_by(
        |a, b| match (crate::is_main(&a.id), crate::is_main(&b.id)) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => b
                .updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id)),
        },
    );
}

#[cfg(test)]
pub(crate) fn recoverable_session_ids_from_summaries(summaries: &[SessionSummary]) -> Vec<String> {
    summaries
        .iter()
        .filter(|summary| !summary.corrupt && summary.messages > 0)
        .map(|summary| summary.id.clone())
        .collect()
}

pub(crate) fn list_saved_session_ids_result(dir: &Path) -> Result<HashSet<String>, String> {
    #[cfg(not(test))]
    {
        let _ = dir;
        crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .list_session_ids_blocking()
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    {
        let mut ids = HashSet::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    ids.insert(stem.to_string());
                }
            }
        }
        Ok(ids)
    }
}

#[cfg(test)]
pub(crate) fn list_saved_session_ids_in_dir(dir: &Path) -> HashSet<String> {
    list_saved_session_ids_result(dir).unwrap_or_default()
}

#[allow(dead_code)]
pub(crate) fn build_history_payload(session: &Session) -> serde_json::Value {
    build_history_payload_with_s3(session, None)
}

pub(crate) fn build_history_payload_with_s3(
    session: &Session,
    s3_cfg: Option<&crate::config::S3Config>,
) -> serde_json::Value {
    let mut msgs = Vec::new();
    let snapshot_lookup =
        normalize_subagent_snapshots_for_messages(&session.messages, &session.subagent_snapshots);
    let mut tool_occurrences: HashMap<String, usize> = HashMap::new();
    let mut tool_names_by_id: HashMap<String, String> = HashMap::new();
    for (message_index, msg) in session.messages.iter().enumerate() {
        match msg.role.as_str() {
            "system" => {}
            "user" => {
                if let Some(c) = &msg.content {
                    let mut entry = json!({
                        "role":"user",
                        "content":c,
                        "timestamp":msg.timestamp,
                        "message_index": message_index,
                    });
                    if let Some(images) = &msg.images {
                        entry["images"] = json!(
                            images
                                .iter()
                                .map(|image| {
                                    let url = if crate::image_uploads::stored_s3_config_matches(
                                        image.s3_config_id.as_deref(),
                                        s3_cfg,
                                    ) {
                                        crate::image_uploads::resolve_image_url(
                                            &image.url,
                                            image.s3_object_key.as_deref(),
                                            s3_cfg,
                                        )
                                        .unwrap_or_else(|_| image.url.clone())
                                    } else {
                                        image.url.clone()
                                    };
                                    json!({"url": url})
                                })
                                .collect::<Vec<_>>()
                        );
                    }
                    msgs.push(entry);
                }
            }
            "assistant" => {
                let has_content = msg.content.as_deref().is_some_and(|c| !c.is_empty());
                let has_thinking = msg.thinking.as_deref().is_some_and(|t| !t.is_empty());
                if has_content || has_thinking {
                    let content_str = msg.content.as_deref().unwrap_or("");
                    let mut entry = json!({
                        "role":"assistant",
                        "content":content_str,
                        "timestamp":msg.timestamp,
                        "message_index": message_index,
                    });
                    if let Some(thinking) = &msg.thinking
                        && !thinking.is_empty()
                    {
                        entry["thinking"] = json!(thinking);
                    }
                    msgs.push(entry);
                }
                if let Some(tcs) = &msg.tool_calls
                    && session.show_tools
                {
                    for tc in tcs {
                        tool_names_by_id.insert(tc.id.clone(), tc.function.name.clone());
                        if tc.function.name == crate::tools::TOOL_NAME_TODOS
                            || matches!(
                                tc.function.name.as_str(),
                                crate::plan::TOOL_NAME_SUBMIT_PLAN
                                    | crate::plan::TOOL_NAME_UPDATE_PLAN
                            )
                        {
                            continue;
                        }
                        msgs.push(json!({
                            "role":"tool_call",
                            "name":tc.function.name,
                            "arguments":crate::tools::display_tool_arguments(
                                &tc.function.name,
                                &tc.function.arguments
                            ),
                            "id":tc.id
                        }));
                    }
                }
            }
            "tool" => {
                if session.show_tools
                    && let Some(c) = &msg.content
                {
                    let tool_call_id = msg.tool_call_id.as_deref().unwrap_or("");
                    if matches!(
                        tool_names_by_id.get(tool_call_id).map(String::as_str),
                        Some(
                            crate::tools::TOOL_NAME_TODOS
                                | crate::plan::TOOL_NAME_SUBMIT_PLAN
                                | crate::plan::TOOL_NAME_UPDATE_PLAN
                        )
                    ) {
                        continue;
                    }
                    let snapshot_key = if tool_call_id.is_empty() {
                        None
                    } else {
                        Some(subagent_snapshot_storage_key(
                            tool_call_id,
                            next_tool_occurrence(&mut tool_occurrences, tool_call_id),
                        ))
                    };
                    let mut entry = json!({
                        "role":"tool_result",
                        "result":c,
                        "id":tool_call_id,
                        "is_error": session.failed_tool_results.contains(tool_call_id),
                    });
                    if let Some(images) = msg.images.as_ref() {
                        let public_images = images
                            .iter()
                            .filter_map(|image| {
                                if !crate::image_uploads::stored_s3_config_matches(
                                    image.s3_config_id.as_deref(),
                                    s3_cfg,
                                ) {
                                    return None;
                                }
                                let url = crate::image_uploads::resolve_image_url(
                                    &image.url,
                                    image.s3_object_key.as_deref(),
                                    s3_cfg,
                                )
                                .ok()?;
                                Some(json!({
                                    "url": url,
                                    "name": image.name.as_deref().unwrap_or("image"),
                                    "mime_type": image.mime_type.as_deref().unwrap_or("image/jpeg"),
                                }))
                            })
                            .collect::<Vec<_>>();
                        if !public_images.is_empty() {
                            entry["images"] = json!(public_images);
                        }
                    }
                    if let Some(snapshot_key) = snapshot_key.as_deref()
                        && let Some(snapshot) = snapshot_lookup.get(snapshot_key)
                    {
                        entry["subagent_snapshot"] =
                            sanitize_subagent_snapshot_for_history_with_s3(snapshot, s3_cfg);
                    }
                    msgs.push(entry);
                }
            }
            _ => {}
        }
    }
    let mut payload = json!({"type":"history","messages":msgs});
    if let Some(plan) = session.pending_plan.as_ref() {
        payload["plans"] = json!([plan.to_live_value()]);
        if plan.status == crate::plan::PlanStatus::Ready {
            payload["pending_plan"] = json!({
                "plan_id": &plan.id,
                "revision": plan.revision,
                "message_index": plan.assistant_plan_message_index,
                "created_at": plan.created_at,
            });
        }
    }
    payload
}

fn sanitize_subagent_snapshot_for_history_with_s3(
    snapshot: &crate::SubagentHistorySnapshot,
    s3_cfg: Option<&crate::config::S3Config>,
) -> serde_json::Value {
    let mut sanitized = snapshot.clone();
    crate::tools::sanitize_subagent_snapshot_tool_args_in_place(&mut sanitized);
    let mut value = json!(sanitized);
    let Some(tool_values) = value
        .get_mut("tools")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return value;
    };
    for (tool_value, tool) in tool_values.iter_mut().zip(sanitized.tools.iter()) {
        let public_images = tool
            .images
            .iter()
            .filter_map(|image| {
                if !crate::image_uploads::stored_s3_config_matches(
                    image.s3_config_id.as_deref(),
                    s3_cfg,
                ) {
                    return None;
                }
                let url = crate::image_uploads::resolve_image_url(
                    &image.url,
                    image.s3_object_key.as_deref(),
                    s3_cfg,
                )
                .ok()?;
                Some(json!({
                    "url": url,
                    "name": image.name.as_deref().unwrap_or("image"),
                    "mime_type": image.mime_type.as_deref().unwrap_or("image/jpeg"),
                }))
            })
            .collect::<Vec<_>>();
        if let Some(object) = tool_value.as_object_mut() {
            if public_images.is_empty() {
                object.remove("images");
            } else {
                object.insert("images".to_string(), json!(public_images));
            }
        }
    }
    value
}

fn sanitize_exec_tool_args_in_session(session: &mut Session) {
    for message in &mut session.messages {
        crate::tools::sanitize_chat_message_tool_calls_in_place(message);
    }
    for snapshot in session.subagent_snapshots.values_mut() {
        crate::tools::sanitize_subagent_snapshot_tool_args_in_place(snapshot);
    }
}

pub(crate) fn build_view_state_payload(session: &Session) -> serde_json::Value {
    json!({
        "type": "view_state",
        "show_tools": session.show_tools,
        "show_reasoning": session.show_reasoning,
        "show_react": session.show_react,
    })
}

pub(crate) fn resolve_session_target(
    target: &str,
    known_ids: &HashSet<String>,
) -> Result<String, String> {
    let target = validate_session_id(target)?;
    if known_ids.contains(target) {
        return Ok(target.to_string());
    }

    let mut matches: Vec<&String> = known_ids
        .iter()
        .filter(|id| id.starts_with(target))
        .collect();
    matches.sort_unstable();
    match matches.len() {
        0 => Err(format!("Session '{}' not found.", target)),
        1 => Ok(matches[0].to_string()),
        _ => Err(format!(
            "Session '{}' is ambiguous. Use a longer ID.",
            target
        )),
    }
}

pub(crate) fn build_active_session_lines(
    sessions: &HashMap<String, Session>,
    active_ids: &HashSet<String>,
    config: &Config,
) -> Vec<String> {
    let mut ids: Vec<&String> = active_ids.iter().collect();
    ids.sort_unstable();

    ids.into_iter()
        .filter_map(|id| {
            let session = sessions.get(id)?;
            let model = session.effective_model(&config.model).to_string();
            let ctx_limit = config.context_limit_for_model(&model);
            let input_budget = context_input_budget_for_model(config, &model);
            let resolved = config.resolve_model(&model);
            let estimated = estimate_tokens_for_provider(resolved.provider, &session.messages);
            let mt_str = resolved
                .max_tokens
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".into());
            Some(format!(
                "  {id}  {}\n    model: {model}  context_est: {estimated}/{input_budget} (limit {ctx_limit})  token_usage_source: in={} out={}  max_tokens: {mt_str}  [active]",
                session.name,
                session.input_token_source,
                session.output_token_source,
            ))
        })
        .collect()
}

pub(crate) fn build_session_status(session: &Session, config: &Config) -> String {
    let model_ref = session.effective_model(&config.model);
    let canonical_model = config
        .canonical_model_ref(model_ref)
        .unwrap_or_else(|_| model_ref.to_string());
    let resolved = config.resolve_model(&canonical_model);
    let ctx_limit = config.context_limit_for_model(&canonical_model);
    let input_budget = context_input_budget_for_model(config, &canonical_model);
    let estimated_tokens = estimate_tokens_for_provider(resolved.provider, &session.messages);
    let model_max_tokens = resolved
        .max_tokens
        .map(format_token_count)
        .unwrap_or_else(|| "-".into());
    let (prompt_cache_hits, prompt_cache_misses) = crate::prompts::system_prompt_cache_metrics();
    let (session_save_writes, session_save_skips) = session_persist_metrics();

    format!(
        "agent: LingClaw\n\
         model: {canonical_model}\n\
         resolved_provider: {}\n\
         resolved_api_base: {}\n\
         resolved_model_id: {}\n\
         max_tokens: {model_max_tokens}\n\
         context_est: {}/{} (limit {})\n\
         token_usage_source: input={} output={}\n\
         think: {}\n\
         react: {}\n\
         tools: {}\n\
         reasoning: {}\n\
         runtime_metrics: prompt_cache={}/{} session_saves(write/skip)={}/{}",
        resolved.provider.label(),
        resolved.api_base,
        resolved.model_id,
        format_token_count(estimated_tokens as u64),
        format_token_count(input_budget as u64),
        format_token_count(ctx_limit as u64),
        session.input_token_source,
        session.output_token_source,
        session.think_level,
        if session.show_react { "on" } else { "off" },
        if session.show_tools { "on" } else { "off" },
        if session.show_reasoning { "on" } else { "off" },
        prompt_cache_hits,
        prompt_cache_misses,
        session_save_writes,
        session_save_skips,
    )
}

pub(crate) fn build_session_usage(session: &Session) -> String {
    let (today_input_tokens, today_output_tokens) = super::current_daily_token_usage(session);
    let total_input_tokens = session.input_tokens;
    let total_output_tokens = session.output_tokens;
    let total_tokens = session.input_tokens.saturating_add(session.output_tokens);

    format!(
        "today_usage_est: # 当前会话今日 token 使用估算\n\tinput_tokens: {}\n\toutput_tokens: {}\n\n\
total_usage_est: # 当前会话累计 token 使用估算\n\ttotal_tokens: {}\n\ttotal_input_tokens: {}\n\ttotal_output_tokens: {}",
        format_token_count(today_input_tokens),
        format_token_count(today_output_tokens),
        format_token_count(total_tokens),
        format_token_count(total_input_tokens),
        format_token_count(total_output_tokens),
    )
}

#[cfg(test)]
pub(crate) fn build_global_today_usage<'a>(
    sessions: impl IntoIterator<Item = &'a Session>,
) -> String {
    let (global_today_input_tokens, global_today_output_tokens) =
        super::accumulate_daily_token_usage(sessions);
    build_global_today_usage_totals(global_today_input_tokens, global_today_output_tokens)
}

pub(crate) fn build_global_today_usage_totals(
    global_today_input_tokens: u64,
    global_today_output_tokens: u64,
) -> String {
    format_usage_block(
        "global_today_usage_est",
        "所有会话今日 token 使用估算",
        global_today_input_tokens,
        global_today_output_tokens,
    )
}

#[cfg(test)]
pub(crate) fn load_saved_sessions_not_in(loaded_ids: &HashSet<String>) -> Vec<Session> {
    let mut sessions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(sessions_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if loaded_ids.contains(id) {
                continue;
            }
            if let Some(session) = load_session_from_disk(id) {
                sessions.push(session);
            }
        }
    }
    sessions
}

pub(crate) fn build_usage_report(session: &Session, global_today_usage: &str) -> String {
    format!("{}\n\n{}", build_session_usage(session), global_today_usage)
}
