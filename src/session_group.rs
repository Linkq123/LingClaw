use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use crate::{config_dir_path, now_epoch};

pub(crate) const GROUP_VERSION: u32 = 1;

const GENERATED_GROUP_ID_LEN: usize = 6;
const GENERATED_GROUP_ID_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
const STALE_GROUP_RUN_ERROR: &str = "Run stopped because the server restarted before completion.";

type GroupPersistGateLock =
    OnceLock<Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>;
static GROUP_PERSIST_GATES: GroupPersistGateLock = OnceLock::new();

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SessionGroup {
    pub(crate) id: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) members: Vec<String>,
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
    let mut guard = group_persist_gates()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    guard
        .entry(group_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
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
        if trimmed.is_empty() {
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
        if canonical_saved_group_id(&id).is_some() {
            continue;
        }
        return Ok(id);
    }
    Err("Failed to generate a unique group id".to_string())
}

pub(crate) fn group_path(id: &str) -> PathBuf {
    groups_dir().join(format!("{id}.json"))
}

fn tmp_group_path(id: &str) -> PathBuf {
    groups_dir().join(format!("{id}.json.tmp"))
}

fn group_file_id(path: &Path) -> Option<String> {
    let file_name = path.file_name().and_then(|name| name.to_str())?;
    let id = file_name
        .strip_suffix(".json")
        .or_else(|| file_name.strip_suffix(".json.tmp"))?;
    validate_group_id(id).ok().map(str::to_string)
}

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

pub(crate) fn load_group_from_path(path: &Path) -> Option<SessionGroup> {
    let data = std::fs::read_to_string(path).ok()?;
    let mut group: SessionGroup = serde_json::from_str(&data).ok()?;
    normalize_group(&mut group);
    Some(group)
}

pub(crate) fn normalize_group(group: &mut SessionGroup) {
    group.members = normalize_members(std::mem::take(&mut group.members));
    group.version = GROUP_VERSION;
    if group.name.trim().is_empty() {
        group.name = group.id.clone();
    }
}

pub(crate) fn load_group_from_disk(id: &str) -> Option<SessionGroup> {
    let id =
        canonical_saved_group_id(id).or_else(|| validate_group_id(id).ok().map(str::to_string))?;
    let path = group_path(&id);
    let tmp_path = tmp_group_path(&id);
    let primary = load_group_from_path(&path);
    let tmp_available = tmp_path.exists();
    match (primary, tmp_available) {
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
    }
}

pub(crate) fn canonical_saved_group_id(id: &str) -> Option<String> {
    let id = validate_group_id(id).ok()?;
    let mut fallback = None;
    for saved_id in saved_group_file_ids() {
        if saved_id == id {
            return Some(saved_id);
        }
        if cfg!(windows) && saved_id.eq_ignore_ascii_case(id) {
            fallback = Some(saved_id);
        }
    }
    fallback
}

pub(crate) fn list_saved_group_summaries() -> Vec<SessionGroupSummary> {
    let mut out = Vec::new();
    for id in saved_group_file_ids() {
        if let Some(group) = load_group_from_disk(&id) {
            out.push(SessionGroupSummary::from_group(&group));
        } else {
            out.push(SessionGroupSummary::corrupt(id));
        }
    }
    sort_group_summaries(&mut out);
    out
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
        let Some(mut group) = load_group_from_disk(&group_id) else {
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
    if let Err(error) = std::fs::read_dir(groups_dir()) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(0);
        }
        return Err(error.to_string());
    }

    recover_stale_group_runs_for_ids(saved_group_file_ids()).await
}

pub(crate) fn sort_group_summaries(summaries: &mut [SessionGroupSummary]) {
    summaries.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
}

pub(crate) async fn save_group_to_disk_locked(group: &SessionGroup) -> Result<(), String> {
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

pub(crate) async fn delete_group_from_disk_locked(id: &str) -> Result<(), String> {
    let id = validate_group_id(id)?.to_string();
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

pub(crate) fn group_to_json(group: &SessionGroup) -> serde_json::Value {
    json!({
        "id": group.id,
        "name": group.name,
        "members": group.members,
        "messages": group.messages,
        "runs": group.runs,
        "created_at": group.created_at,
        "updated_at": group.updated_at,
        "version": group.version,
    })
}

pub(crate) fn group_history_payload(group: &SessionGroup) -> serde_json::Value {
    json!({
        "type": "group_history",
        "group_id": group.id,
        "members": group.members,
        "messages": group.messages,
        "runs": group.runs,
    })
}

pub(crate) fn group_info_payload(group: &SessionGroup) -> serde_json::Value {
    json!({
        "type": "group",
        "id": group.id,
        "name": group.name,
        "members": group.members,
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

    #[test]
    fn normalize_members_dedupes_and_drops_invalid_ids() {
        let members = normalize_members(vec![
            " alpha ".to_string(),
            "alpha".to_string(),
            "bad/name".to_string(),
            "beta.1".to_string(),
            "".to_string(),
        ]);

        assert_eq!(members, vec!["alpha".to_string(), "beta.1".to_string()]);
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
