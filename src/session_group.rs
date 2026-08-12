use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, OnceLock},
};

#[cfg(test)]
use std::path::{Path, PathBuf};

use crate::{MAIN_SESSION_ID, now_epoch};

#[cfg(test)]
use crate::config_dir_path;

pub(crate) const GROUP_VERSION: u32 = 2;

const GENERATED_GROUP_ID_LEN: usize = 6;
const GENERATED_GROUP_ID_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const STALE_GROUP_RUN_ERROR: &str = "Run stopped because the server restarted before completion.";

type GroupPersistGateLock =
    OnceLock<Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;
static GROUP_PERSIST_GATES: GroupPersistGateLock = OnceLock::new();
static GROUP_ROSTER_GATE: OnceLock<Arc<tokio::sync::Mutex<()>>> = OnceLock::new();
static GROUP_FEATURE_GATE: OnceLock<tokio::sync::RwLock<()>> = OnceLock::new();

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SessionGroup {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) members: Vec<String>,
    #[serde(default)]
    pub(crate) admins: Vec<String>,
    #[serde(default)]
    pub(crate) pending_votes: Vec<GroupVote>,
    #[serde(default)]
    pub(crate) messages: Vec<GroupMessage>,
    #[serde(default)]
    pub(crate) runs: Vec<GroupRun>,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    #[serde(default)]
    pub(crate) version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GroupVote {
    pub(crate) id: String,
    pub(crate) action: String,
    pub(crate) target_session_id: String,
    pub(crate) requester_session_id: String,
    #[serde(default)]
    pub(crate) approvals: Vec<String>,
    pub(crate) threshold: usize,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GroupMemberDetail {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) role: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GroupMessage {
    pub(crate) id: String,
    pub(crate) role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) session_id: Option<String>,
    pub(crate) content: String,
    pub(crate) timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) run_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GroupRun {
    pub(crate) id: String,
    pub(crate) group_id: String,
    pub(crate) session_id: String,
    pub(crate) status: String,
    pub(crate) prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) result_excerpt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) completed_at: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SessionGroupSummary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) members: usize,
    pub(crate) messages: usize,
    pub(crate) running: usize,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) corrupt: bool,
}

impl SessionGroup {
    pub(crate) fn new(id: &str, name: &str, members: Vec<String>) -> Self {
        let now = now_epoch();
        Self {
            id: id.to_string(),
            name: name.to_string(),
            members: normalize_members(members),
            admins: Vec::new(),
            pending_votes: Vec::new(),
            messages: Vec::new(),
            runs: Vec::new(),
            created_at: now,
            updated_at: now,
            version: GROUP_VERSION,
        }
    }
}

impl SessionGroupSummary {
    pub(crate) fn from_group(group: &SessionGroup) -> Self {
        Self {
            id: group.id.clone(),
            name: group.name.clone(),
            members: group.members.len(),
            messages: group.messages.len(),
            running: group
                .runs
                .iter()
                .filter(|run| is_active_group_run_status(&run.status))
                .count(),
            created_at: group.created_at,
            updated_at: group.updated_at,
            corrupt: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn corrupt(id: String) -> Self {
        Self {
            id,
            name: "[Corrupt Group]".to_string(),
            members: 0,
            messages: 0,
            running: 0,
            created_at: 0,
            updated_at: 0,
            corrupt: true,
        }
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        json!({
            "id": self.id,
            "name": self.name,
            "members": self.members,
            "messages": self.messages,
            "running": self.running,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "corrupt": self.corrupt,
        })
    }
}

pub(crate) fn is_active_group_run_status(status: &str) -> bool {
    matches!(status, "queued" | "running")
}

pub(crate) fn group_has_active_runs(group: &SessionGroup) -> bool {
    group
        .runs
        .iter()
        .any(|run| is_active_group_run_status(&run.status))
}

#[cfg(test)]
pub(crate) fn groups_dir() -> PathBuf {
    config_dir_path()
        .unwrap_or_else(|| PathBuf::from(".lingclaw"))
        .join("groups")
}

fn group_persist_gates()
-> &'static Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    GROUP_PERSIST_GATES.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

pub(crate) fn group_persist_gate(group_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let group_id = validate_group_id(group_id).unwrap_or(group_id.trim());
    let group_id = if cfg!(windows) {
        group_id.to_ascii_lowercase()
    } else {
        group_id.to_string()
    };
    let mut guard = group_persist_gates()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    guard
        .entry(group_id)
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

pub(crate) fn group_roster_gate() -> Arc<tokio::sync::Mutex<()>> {
    GROUP_ROSTER_GATE
        .get_or_init(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Serializes Group feature transitions against Group reads and mutations.
/// Normal operations take a read guard; configuration saves take the write
/// guard before changing `enableGroups` and keep it through hot-disable.
pub(crate) fn group_feature_gate() -> &'static tokio::sync::RwLock<()> {
    GROUP_FEATURE_GATE.get_or_init(|| tokio::sync::RwLock::new(()))
}

pub(crate) fn validate_group_id(id: &str) -> Result<&str, String> {
    let trimmed = crate::session_store::validate_session_id(id)?;
    if matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "groups" | "group" | "session-groups"
    ) {
        return Err("Invalid group id. This name is reserved.".to_string());
    }
    Ok(trimmed)
}

pub(crate) fn validate_group_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Group name cannot be empty.".to_string());
    }
    if trimmed.chars().count() > 80 {
        return Err("Group name must be 80 characters or fewer.".to_string());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("Group name cannot contain control characters.".to_string());
    }
    Ok(trimmed.to_string())
}

pub(crate) fn normalize_members(members: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for member in members {
        let trimmed = member.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(MAIN_SESSION_ID) {
            continue;
        }
        let Ok(valid) = crate::session_store::validate_session_id(trimmed) else {
            continue;
        };
        if seen.insert(valid.to_string()) {
            out.push(valid.to_string());
        }
    }
    out
}

pub(crate) fn normalize_admins(admins: Vec<String>, members: &[String]) -> Vec<String> {
    let member_set = members.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for admin in admins {
        let trimmed = admin.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(MAIN_SESSION_ID) {
            continue;
        }
        let Ok(valid) = crate::session_store::validate_session_id(trimmed) else {
            continue;
        };
        if member_set.contains(&valid) && seen.insert(valid.to_string()) {
            out.push(valid.to_string());
        }
    }
    out
}

pub(crate) fn normalize_vote_approvals(approvals: Vec<String>, admins: &[String]) -> Vec<String> {
    let admin_set = admins.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    approvals
        .into_iter()
        .filter_map(|approval| {
            let valid = crate::session_store::validate_session_id(approval.trim()).ok()?;
            if admin_set.contains(&valid) && seen.insert(valid.to_string()) {
                Some(valid.to_string())
            } else {
                None
            }
        })
        .collect()
}

pub(crate) fn generate_random_group_id() -> Result<String, String> {
    let mut id = String::with_capacity(GENERATED_GROUP_ID_LEN);
    while id.len() < GENERATED_GROUP_ID_LEN {
        let mut bytes = [0_u8; 16];
        getrandom::getrandom(&mut bytes)
            .map_err(|error| format!("failed to generate group id: {error}"))?;
        for byte in bytes {
            if byte >= 252 {
                continue;
            }
            let idx = (byte % GENERATED_GROUP_ID_ALPHABET.len() as u8) as usize;
            id.push(GENERATED_GROUP_ID_ALPHABET[idx] as char);
            if id.len() == GENERATED_GROUP_ID_LEN {
                break;
            }
        }
    }
    Ok(id)
}

pub(crate) fn generate_available_group_id() -> Result<String, String> {
    for _ in 0..128 {
        let id = generate_random_group_id()?;
        if validate_group_id(&id).is_err() {
            continue;
        }
        if canonical_saved_group_id_result(&id)?.is_some() {
            continue;
        }
        return Ok(id);
    }
    Err("Failed to generate a unique group id".to_string())
}

#[cfg(test)]
pub(crate) fn group_path(id: &str) -> PathBuf {
    groups_dir().join(format!("{id}.json"))
}

#[cfg(test)]
fn tmp_group_path(id: &str) -> PathBuf {
    groups_dir().join(format!("{id}.json.tmp"))
}

#[cfg(test)]
fn group_file_id(path: &Path) -> Option<String> {
    let file_name = path.file_name().and_then(|name| name.to_str())?;
    let id = file_name
        .strip_suffix(".json")
        .or_else(|| file_name.strip_suffix(".json.tmp"))?;
    validate_group_id(id).ok().map(str::to_string)
}

#[cfg(test)]
fn saved_group_file_ids() -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(groups_dir()) {
        for entry in entries.flatten() {
            let Some(id) = group_file_id(&entry.path()) else {
                continue;
            };
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
    }
    out
}

#[cfg(test)]
pub(crate) fn load_group_from_path(path: &Path) -> Option<SessionGroup> {
    let data = std::fs::read_to_string(path).ok()?;
    let mut group: SessionGroup = serde_json::from_str(&data).ok()?;
    normalize_group(&mut group);
    Some(group)
}

pub(crate) fn normalize_group(group: &mut SessionGroup) {
    group.members = normalize_members(std::mem::take(&mut group.members));
    group.admins = normalize_admins(std::mem::take(&mut group.admins), &group.members);
    let member_set = group.members.iter().collect::<HashSet<_>>();
    let admins = group.admins.clone();
    group.pending_votes = std::mem::take(&mut group.pending_votes)
        .into_iter()
        .filter_map(|mut vote| {
            if vote.action != "remove_member" {
                return None;
            }
            if !member_set.contains(&vote.target_session_id) {
                return None;
            }
            vote.approvals = normalize_vote_approvals(vote.approvals, &admins);
            vote.threshold = vote.threshold.max(1);
            if vote.approvals.is_empty() {
                None
            } else {
                Some(vote)
            }
        })
        .collect();
    group.version = GROUP_VERSION;
    if group.name.trim().is_empty() {
        group.name = group.id.clone();
    }
}

pub(crate) fn load_group_from_storage_result(id: &str) -> Result<Option<SessionGroup>, String> {
    #[cfg(not(test))]
    {
        let Some(id) = canonical_saved_group_id_result(id)? else {
            return Ok(None);
        };
        crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .load_group_blocking(&id)
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    {
        let Some(id) =
            canonical_saved_group_id(id).or_else(|| validate_group_id(id).ok().map(str::to_string))
        else {
            return Ok(None);
        };
        let path = group_path(&id);
        let tmp_path = tmp_group_path(&id);
        let primary = load_group_from_path(&path);
        let tmp_available = tmp_path.exists();
        Ok(match (primary, tmp_available) {
            (Some(group), false) => Some(group),
            (None, true) => load_group_from_path(&tmp_path),
            (Some(group), true) => {
                let primary_mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                let tmp_mtime = std::fs::metadata(&tmp_path).and_then(|m| m.modified()).ok();
                if tmp_mtime >= primary_mtime {
                    load_group_from_path(&tmp_path).or(Some(group))
                } else {
                    let _ = std::fs::remove_file(tmp_path);
                    Some(group)
                }
            }
            (None, false) => None,
        })
    }
}

#[cfg(test)]
pub(crate) fn load_group_from_disk(id: &str) -> Option<SessionGroup> {
    load_group_from_storage_result(id).ok().flatten()
}

pub(crate) fn canonical_saved_group_id_result(id: &str) -> Result<Option<String>, String> {
    #[cfg(not(test))]
    {
        let id = validate_group_id(id)?;
        crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .canonical_group_id_blocking(id)
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    {
        let id = match validate_group_id(id) {
            Ok(id) => id,
            Err(_) => return Ok(None),
        };
        let mut fallback = None;
        for saved_id in saved_group_file_ids() {
            if saved_id == id {
                return Ok(Some(saved_id));
            }
            if cfg!(windows) && saved_id.eq_ignore_ascii_case(id) {
                fallback = Some(saved_id);
            }
        }
        Ok(fallback)
    }
}

#[cfg(test)]
pub(crate) fn canonical_saved_group_id(id: &str) -> Option<String> {
    canonical_saved_group_id_result(id).ok().flatten()
}

pub(crate) fn list_saved_group_summaries_result() -> Result<Vec<SessionGroupSummary>, String> {
    #[cfg(not(test))]
    {
        crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .list_group_summaries_blocking()
            .map_err(|error| error.to_string())
    }

    #[cfg(test)]
    {
        let mut out = Vec::new();
        for id in saved_group_file_ids() {
            if let Some(group) = load_group_from_disk(&id) {
                out.push(SessionGroupSummary::from_group(&group));
            } else {
                out.push(SessionGroupSummary::corrupt(id));
            }
        }
        sort_group_summaries(&mut out);
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) fn list_saved_group_summaries() -> Vec<SessionGroupSummary> {
    list_saved_group_summaries_result().unwrap_or_default()
}

fn mark_stale_active_runs(group: &mut SessionGroup, now: u64) -> usize {
    let mut changed = 0usize;
    for run in &mut group.runs {
        if !is_active_group_run_status(&run.status) {
            continue;
        }
        run.status = "stopped".to_string();
        run.error = Some(STALE_GROUP_RUN_ERROR.to_string());
        run.updated_at = now;
        run.completed_at = Some(now);
        changed += 1;
    }
    if changed > 0 {
        group.updated_at = now;
    }
    changed
}

async fn recover_stale_group_runs_for_ids(group_ids: Vec<String>) -> Result<usize, String> {
    let mut recovered = 0usize;
    for group_id in group_ids {
        let gate = group_persist_gate(&group_id);
        let _guard = gate.lock().await;
        let Some(mut group) = load_group_from_storage_result(&group_id)? else {
            continue;
        };
        let changed = mark_stale_active_runs(&mut group, now_epoch());
        if changed == 0 {
            continue;
        }
        save_group_to_disk_locked(&group).await?;
        recovered += changed;
    }
    Ok(recovered)
}

pub(crate) async fn recover_stale_group_runs_on_startup() -> Result<usize, String> {
    #[cfg(not(test))]
    {
        let ids = crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .list_group_ids()
            .await
            .map_err(|error| error.to_string())?;
        recover_stale_group_runs_for_ids(ids).await
    }

    #[cfg(test)]
    {
        if let Err(error) = std::fs::read_dir(groups_dir()) {
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(0);
            }
            return Err(error.to_string());
        }

        recover_stale_group_runs_for_ids(saved_group_file_ids()).await
    }
}

#[cfg(test)]
pub(crate) fn sort_group_summaries(summaries: &mut [SessionGroupSummary]) {
    summaries.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
}

pub(crate) async fn save_group_to_disk_locked(group: &SessionGroup) -> Result<(), String> {
    #[cfg(not(test))]
    {
        return crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .save_group(group)
            .await
            .map_err(|error| error.to_string());
    }

    #[cfg(test)]
    {
        tokio::fs::create_dir_all(groups_dir())
            .await
            .map_err(|error| error.to_string())?;
        let payload = serde_json::to_string(group).map_err(|error| error.to_string())?;
        let tmp = tmp_group_path(&group.id);
        let target = group_path(&group.id);
        tokio::fs::write(&tmp, payload)
            .await
            .map_err(|error| error.to_string())?;
        if let Err(error) = crate::session_store::replace_session_file_from_temp(&target, &tmp) {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(error);
        }
        Ok(())
    }
}

pub(crate) async fn delete_group_from_disk_locked(id: &str) -> Result<(), String> {
    let id = canonical_saved_group_id_result(id)?
        .or_else(|| validate_group_id(id).ok().map(str::to_string))
        .ok_or_else(|| "Invalid group id.".to_string())?;
    #[cfg(not(test))]
    {
        crate::storage::Database::global()
            .map_err(|error| error.to_string())?
            .delete_group(&id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[cfg(test)]
    {
        let path = group_path(&id);
        let tmp = tmp_group_path(&id);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        match tokio::fs::remove_file(&tmp).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        Ok(())
    }
}

/// Content-derived fingerprint of one persisted session file. Including the parsed
/// display name and `updated_at` (instead of just filesystem mtime/len) guarantees that
/// a rename always changes the signature, even when the serialized byte length is
/// unchanged within the same coarse filesystem mtime tick.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg(test)]
struct SavedSessionNameSignature {
    id: String,
    name: String,
    updated_at: u64,
    corrupt: bool,
}

#[derive(Clone, Debug)]
#[cfg(test)]
struct SavedSessionNameCache {
    signatures: Vec<SavedSessionNameSignature>,
    names: HashMap<String, String>,
}

#[cfg(test)]
type SavedSessionNameCacheLock = OnceLock<Mutex<Option<SavedSessionNameCache>>>;
#[cfg(test)]
static SAVED_SESSION_NAME_CACHE: SavedSessionNameCacheLock = OnceLock::new();

/// Build the content-derived signature from already-parsed session summaries. Pure (no I/O)
/// so it can be unit-tested directly without touching the global sessions dir or cache.
#[cfg(test)]
fn signatures_from_summaries(
    summaries: &[crate::session_store::SessionSummary],
) -> Vec<SavedSessionNameSignature> {
    let mut signatures = summaries
        .iter()
        .map(|summary| SavedSessionNameSignature {
            id: summary.id.clone(),
            name: summary.name.clone(),
            updated_at: summary.updated_at,
            corrupt: summary.corrupt,
        })
        .collect::<Vec<_>>();
    signatures.sort_by(|a, b| a.id.cmp(&b.id));
    signatures
}

pub(crate) fn saved_session_name_map() -> HashMap<String, String> {
    #[cfg(not(test))]
    {
        crate::storage::Database::global()
            .and_then(|database| database.session_name_map_blocking())
            .unwrap_or_default()
    }

    #[cfg(test)]
    {
        // Parse the sessions directory once; derive both the content signature and the name
        // map from the same summaries so renames (even same-length, same-mtime-tick) always
        // invalidate the cache, and so no second directory pass is performed on a miss.
        let summaries = crate::session_store::list_saved_session_summaries_in_dir(
            &crate::session_store::sessions_dir(),
        );
        let signatures = signatures_from_summaries(&summaries);
        let cache = SAVED_SESSION_NAME_CACHE.get_or_init(|| Mutex::new(None));
        {
            let guard = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = guard.as_ref()
                && cached.signatures == signatures
            {
                return cached.names.clone();
            }
        }
        let names = summaries
            .into_iter()
            .map(|summary| (summary.id, summary.name))
            .collect::<HashMap<_, _>>();
        {
            let mut guard = cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = Some(SavedSessionNameCache {
                signatures,
                names: names.clone(),
            });
        }
        names
    }
}

pub(crate) fn group_member_details(
    group: &SessionGroup,
    names: &HashMap<String, String>,
) -> Vec<GroupMemberDetail> {
    let mut details = Vec::with_capacity(group.members.len() + 1);
    details.push(GroupMemberDetail {
        id: MAIN_SESSION_ID.to_string(),
        name: names
            .get(MAIN_SESSION_ID)
            .cloned()
            .unwrap_or_else(|| "Main".to_string()),
        role: "owner".to_string(),
    });
    for member in &group.members {
        details.push(GroupMemberDetail {
            id: member.clone(),
            name: names.get(member).cloned().unwrap_or_else(|| member.clone()),
            role: if group.admins.iter().any(|admin| admin == member) {
                "admin".to_string()
            } else {
                "member".to_string()
            },
        });
    }
    details
}

pub(crate) fn group_to_json(
    group: &SessionGroup,
    names: &HashMap<String, String>,
) -> serde_json::Value {
    json!({
        "id": group.id,
        "name": group.name,
        "members": group.members,
        "admins": group.admins,
        "pending_votes": group.pending_votes,
        "member_details": group_member_details(group, names),
        "messages": group.messages,
        "runs": group.runs,
        "created_at": group.created_at,
        "updated_at": group.updated_at,
        "version": group.version,
    })
}

pub(crate) fn group_history_payload(
    group: &SessionGroup,
    names: &HashMap<String, String>,
) -> serde_json::Value {
    json!({
        "type": "group_history",
        "group_id": group.id,
        "members": group.members,
        "admins": group.admins,
        "pending_votes": group.pending_votes,
        "member_details": group_member_details(group, names),
        "messages": group.messages,
        "runs": group.runs,
    })
}

pub(crate) fn group_info_payload(
    group: &SessionGroup,
    names: &HashMap<String, String>,
) -> serde_json::Value {
    json!({
        "type": "group",
        "id": group.id,
        "name": group.name,
        "members": group.members,
        "admins": group.admins,
        "pending_votes": group.pending_votes,
        "member_details": group_member_details(group, names),
        "created_at": group.created_at,
        "updated_at": group.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_group_id(prefix: &str) -> String {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        format!("{prefix}-{unique}")
    }

    fn summary(id: &str, name: &str, updated_at: u64) -> crate::session_store::SessionSummary {
        crate::session_store::SessionSummary {
            id: id.to_string(),
            name: name.to_string(),
            model_override: None,
            messages: 0,
            tool_calls: 0,
            created_at: 0,
            updated_at,
            corrupt: false,
            workspace_kind: crate::SessionWorkspaceKind::Managed,
            working_directory: std::path::PathBuf::new(),
        }
    }

    #[test]
    fn saved_session_name_signature_changes_when_name_changes_at_same_updated_at() {
        // Same id + same updated_at + same length name: a rename that the old
        // (file_name, mtime, len) signature could not distinguish.
        let before = signatures_from_summaries(&[summary("worker-a", "Cat", 5)]);
        let after = signatures_from_summaries(&[summary("worker-a", "Dog", 5)]);
        assert_ne!(before, after);
    }

    #[test]
    fn saved_session_name_signature_is_stable_and_order_independent() {
        let a = signatures_from_summaries(&[summary("b", "Beta", 2), summary("a", "Alpha", 1)]);
        let b = signatures_from_summaries(&[summary("a", "Alpha", 1), summary("b", "Beta", 2)]);
        assert_eq!(a, b);
    }

    #[test]
    fn normalize_members_dedupes_and_drops_invalid_ids() {
        let members = normalize_members(vec![
            " alpha ".to_string(),
            "alpha".to_string(),
            "main".to_string(),
            "MAIN".to_string(),
            "bad/name".to_string(),
            "beta.1".to_string(),
            "".to_string(),
        ]);

        assert_eq!(members, vec!["alpha".to_string(), "beta.1".to_string()]);
    }

    #[test]
    fn normalize_group_defaults_v2_metadata_and_injects_main_owner_detail() {
        let group_id = unique_group_id("group-v1-load");
        let mut group: SessionGroup = serde_json::from_value(json!({
            "id": group_id,
            "name": "Legacy Group",
            "members": ["main", "worker-a", "worker-a"],
            "messages": [],
            "runs": [],
            "created_at": 1,
            "updated_at": 1,
            "version": 1
        }))
        .expect("legacy group should deserialize");

        normalize_group(&mut group);

        assert_eq!(group.version, GROUP_VERSION);
        assert_eq!(group.members, vec!["worker-a".to_string()]);
        assert!(group.admins.is_empty());
        assert!(group.pending_votes.is_empty());
        let details = group_member_details(&group, &HashMap::new());
        assert_eq!(details[0].id, MAIN_SESSION_ID);
        assert_eq!(details[0].role, "owner");
        assert_eq!(details[1].id, "worker-a");
        assert_eq!(details[1].role, "member");
    }

    #[test]
    fn normalize_group_keeps_vote_when_requester_is_no_longer_admin() {
        let mut group = SessionGroup::new(
            "group-vote-normalize",
            "Vote Normalize",
            vec![
                "worker-a".to_string(),
                "worker-b".to_string(),
                "worker-c".to_string(),
            ],
        );
        group.admins = vec!["worker-a".to_string()];
        group.pending_votes.push(GroupVote {
            id: "vote-1".to_string(),
            action: "remove_member".to_string(),
            target_session_id: "worker-c".to_string(),
            requester_session_id: "worker-b".to_string(),
            approvals: vec!["worker-a".to_string(), "worker-b".to_string()],
            threshold: 2,
            created_at: 1,
            updated_at: 1,
        });

        normalize_group(&mut group);

        assert_eq!(group.pending_votes.len(), 1);
        assert_eq!(group.pending_votes[0].requester_session_id, "worker-b");
        assert_eq!(group.pending_votes[0].approvals, vec!["worker-a"]);
    }

    #[test]
    fn mark_stale_active_runs_stops_only_nonterminal_runs() {
        let mut group = SessionGroup::new("group-stale-test", "Review Group", Vec::new());
        group.runs = vec![
            GroupRun {
                id: "queued-run".to_string(),
                group_id: group.id.clone(),
                session_id: "worker-a".to_string(),
                status: "queued".to_string(),
                prompt: "inspect".to_string(),
                result_excerpt: None,
                error: None,
                created_at: 1,
                updated_at: 1,
                completed_at: None,
            },
            GroupRun {
                id: "running-run".to_string(),
                group_id: group.id.clone(),
                session_id: "worker-b".to_string(),
                status: "running".to_string(),
                prompt: "inspect".to_string(),
                result_excerpt: None,
                error: None,
                created_at: 1,
                updated_at: 1,
                completed_at: None,
            },
            GroupRun {
                id: "done-run".to_string(),
                group_id: group.id.clone(),
                session_id: "worker-c".to_string(),
                status: "completed".to_string(),
                prompt: "inspect".to_string(),
                result_excerpt: Some("done".to_string()),
                error: None,
                created_at: 1,
                updated_at: 2,
                completed_at: Some(2),
            },
        ];

        assert_eq!(mark_stale_active_runs(&mut group, 10), 2);
        assert_eq!(group.updated_at, 10);
        assert_eq!(SessionGroupSummary::from_group(&group).running, 0);
        assert!(group.runs[..2].iter().all(|run| {
            run.status == "stopped"
                && run.error.as_deref() == Some(STALE_GROUP_RUN_ERROR)
                && run.completed_at == Some(10)
                && run.updated_at == 10
        }));
        assert_eq!(group.runs[2].status, "completed");
        assert_eq!(group.runs[2].completed_at, Some(2));
    }

    #[tokio::test]
    async fn save_load_list_and_delete_group_round_trip() {
        let group_id = unique_group_id("group-store-test");
        let group = SessionGroup::new(
            &group_id,
            "Review Group",
            vec!["worker-a".to_string(), "worker-b".to_string()],
        );
        let gate = group_persist_gate(&group_id);
        let _guard = gate.lock().await;

        save_group_to_disk_locked(&group)
            .await
            .expect("group should save");

        let loaded = load_group_from_disk(&group_id).expect("group should load");
        assert_eq!(loaded.id, group_id);
        assert_eq!(loaded.name, "Review Group");
        assert_eq!(loaded.members, vec!["worker-a", "worker-b"]);

        let summaries = list_saved_group_summaries();
        assert!(summaries.iter().any(|summary| summary.id == group_id));

        delete_group_from_disk_locked(&group_id)
            .await
            .expect("group should delete");
        assert!(load_group_from_disk(&group_id).is_none());
    }

    #[tokio::test]
    async fn startup_recovery_handles_tmp_only_group_files() {
        let group_id = unique_group_id("group-tmp-recovery");
        let mut group = SessionGroup::new(&group_id, "Review Group", vec!["worker-a".to_string()]);
        group.runs.push(GroupRun {
            id: "queued-run".to_string(),
            group_id: group_id.clone(),
            session_id: "worker-a".to_string(),
            status: "queued".to_string(),
            prompt: "inspect".to_string(),
            result_excerpt: None,
            error: None,
            created_at: 1,
            updated_at: 1,
            completed_at: None,
        });
        tokio::fs::create_dir_all(groups_dir())
            .await
            .expect("groups dir should be created");
        tokio::fs::write(
            tmp_group_path(&group_id),
            serde_json::to_string(&group).expect("group should serialize"),
        )
        .await
        .expect("tmp group should be written");

        let recovered = recover_stale_group_runs_for_ids(vec![group_id.clone()])
            .await
            .expect("startup recovery should succeed");
        assert_eq!(recovered, 1);

        let loaded = load_group_from_disk(&group_id).expect("group should load from recovered tmp");
        assert_eq!(loaded.runs[0].status, "stopped");
        assert_eq!(SessionGroupSummary::from_group(&loaded).running, 0);

        delete_group_from_disk_locked(&group_id)
            .await
            .expect("group should delete");
    }
}
