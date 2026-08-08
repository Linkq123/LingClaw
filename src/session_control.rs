use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
};
use futures::FutureExt;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs::Metadata,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    AppState, ChatMessage, Config, MAIN_SESSION_ID, Session, now_epoch,
    prompts::{self, SkillSource},
    runtime_loop::{self, AgentRunMode},
    session_group::{self, GroupMessage, GroupRun, SessionGroup},
    session_store::{self, SessionSummary},
    tools,
};

static NEXT_CONTROL_ID: AtomicU64 = AtomicU64::new(1);

const CREATE_SESSION_PROFILE_MAX_CHARS: usize = 4_000;
const CREATE_SESSION_AGENT_NOTES_MAX_CHARS: usize = 8_000;
const SESSION_CONTROL_MESSAGE_MAX_CHARS: usize = 32_000;
const SESSION_CONTROL_TARGETS_MAX_ITEMS: usize = 16;
const SESSION_CONTROL_MEMBERS_MAX_ITEMS: usize = 64;
const SESSION_CONTROL_SECTIONS_MAX_ITEMS: usize = 4;
static DIRECT_RUNS: std::sync::LazyLock<std::sync::Mutex<HashMap<String, DirectRunEntry>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
static GROUP_RUN_CONTROLS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<String, GroupRunControlEntry>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

const DIRECT_RUN_RETAIN_SECS: u64 = 10 * 60;

fn storage_protected_control_error() -> String {
    "session_control error: local storage is in protected mode; repair it and restart LingClaw."
        .to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectRunStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Stopped,
}

impl DirectRunStatus {
    fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }

    fn is_terminal(self) -> bool {
        !self.is_active()
    }
}

#[derive(Clone, Debug)]
struct DelegatedRunControl {
    cancel: CancellationToken,
    stop_requested: Arc<AtomicBool>,
}

#[derive(Clone, Debug)]
struct DirectRunEntry {
    session_id: String,
    status: DirectRunStatus,
    cancel: CancellationToken,
    stop_requested: Arc<AtomicBool>,
    updated_at: u64,
}

#[derive(Clone, Debug)]
struct GroupRunControlEntry {
    group_id: String,
    session_id: String,
    cancel: CancellationToken,
    stop_requested: Arc<AtomicBool>,
}

#[derive(Clone, Debug)]
pub(crate) struct GroupSocketDispatch {
    pub(crate) text: String,
    pub(crate) targets: Vec<String>,
    pub(crate) target_mode: String,
    pub(crate) start_runs: bool,
    pub(crate) run_mode: String,
}

#[derive(Clone, Debug)]
struct DispatchRequest {
    group_id: Option<String>,
    targets: Vec<String>,
    optional_targets: HashSet<String>,
    message: String,
    group_message: Option<DispatchGroupMessage>,
    run_mode: AgentRunMode,
    wait: bool,
    summary_budget: usize,
    mention_depth: u8,
}

#[derive(Clone, Debug)]
struct DispatchGroupMessage {
    role: String,
    session_id: Option<String>,
    turn_id: Option<String>,
}

#[derive(Clone, Debug)]
struct StartedRun {
    run_id: String,
    group_id: Option<String>,
    session_id: String,
    optional_reply: bool,
    mention_depth: u8,
    control: DelegatedRunControl,
}

fn next_id(prefix: &str) -> String {
    let n = NEXT_CONTROL_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{:x}_{:x}", now_epoch(), n)
}

fn parse_run_mode(value: &str) -> Result<AgentRunMode, String> {
    match value {
        "execute" | "" => Ok(AgentRunMode::Execute),
        "plan_only" | "plan" => Ok(AgentRunMode::PlanOnly),
        other => Err(format!(
            "Invalid run_mode '{}'. Use 'execute' or 'plan_only'.",
            other
        )),
    }
}

fn normalize_target_ids(targets: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for target in targets {
        let trimmed = target.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(valid) = session_store::validate_session_id(trimmed) else {
            continue;
        };
        if seen.insert(valid.to_string()) {
            out.push(valid.to_string());
        }
    }
    out
}

fn validate_group_targets(
    group_id: &str,
    members: &[String],
    targets: &[String],
) -> Result<(), String> {
    let member_set = members.iter().collect::<HashSet<_>>();
    let non_members = targets
        .iter()
        .filter(|target| {
            !(member_set.contains(*target)
                || members
                    .iter()
                    .any(|member| crate::session_ids_match(member, target)))
        })
        .cloned()
        .collect::<Vec<_>>();
    if non_members.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Target session(s) are not members of group {group_id}: {}",
            non_members.join(", ")
        ))
    }
}

async fn resolve_existing_target_session_ids(
    state: &Arc<AppState>,
    targets: Vec<String>,
) -> Result<Vec<String>, String> {
    let mut resolved_targets = Vec::new();
    let mut seen = HashSet::new();
    for target in targets {
        let loaded_id = {
            let sessions = state.sessions.lock().await;
            crate::find_loaded_session_id(&sessions, &target)
        };
        if let Some(session_id) = loaded_id {
            if crate::is_main(&session_id) {
                return Err(
                    "session_control error: cannot dispatch to the main session from session_control."
                        .to_string(),
                );
            }
            if seen.insert(session_id.clone()) {
                resolved_targets.push(session_id);
            }
            continue;
        }

        let Some(session) = session_store::load_session_from_storage_result(&target)? else {
            return Err(format!(
                "Session '{}' not found. Create it first with session_control.create_session.",
                target
            ));
        };
        let session_id = session.id.clone();
        if crate::is_main(&session_id) {
            return Err(
                "session_control error: cannot dispatch to the main session from session_control."
                    .to_string(),
            );
        }
        let effective_id = {
            let mut sessions = state.sessions.lock().await;
            let effective_id =
                crate::find_loaded_session_id(&sessions, &session_id).unwrap_or(session_id);
            if crate::is_main(&effective_id) {
                return Err(
                    "session_control error: cannot dispatch to the main session from session_control."
                        .to_string(),
                );
            }
            sessions.entry(effective_id.clone()).or_insert(session);
            effective_id
        };
        if seen.insert(effective_id.clone()) {
            resolved_targets.push(effective_id);
        }
    }
    Ok(resolved_targets)
}

async fn prepare_dispatch_targets(
    state: &Arc<AppState>,
    group_id: Option<&str>,
    mut targets: Vec<String>,
    max_targets: usize,
) -> Result<Vec<String>, String> {
    let group_members = if let Some(group_id) = group_id {
        let group = session_group::load_group_from_storage_result(group_id)?
            .ok_or_else(|| format!("Group '{}' not found", group_id))?;
        Some(group.members)
    } else {
        None
    };
    if targets.is_empty()
        && let Some(members) = group_members.as_ref()
    {
        targets = members.clone();
    }
    let targets = normalize_target_ids(targets);
    if targets.is_empty() {
        return Err("No target sessions were selected.".to_string());
    }
    if let Some(members) = group_members.as_ref() {
        validate_group_targets(group_id.unwrap_or_default(), members, &targets)?;
    }
    if targets.iter().any(|target| crate::is_main(target)) {
        return Err(
            "session_control error: cannot dispatch to the main session from session_control."
                .to_string(),
        );
    }
    if targets.len() > max_targets {
        return Err(format!(
            "session_control error: targets exceeds {} item(s)",
            max_targets
        ));
    }
    resolve_existing_target_session_ids(state, targets).await
}

async fn ensure_explicit_target_models(state: &AppState, targets: &[String]) -> Result<(), String> {
    let model_status_guard = crate::CONFIG_FILE_LOCK.read().await;
    let config = state.config();
    let sessions = state.sessions.lock().await;
    let missing = target_sessions_missing_explicit_models(&config, &sessions, targets);
    drop(sessions);
    drop(model_status_guard);
    if !missing.is_empty() {
        return Err(format!(
            "Explicit model configuration is required for target session(s): {}.",
            missing.join(", ")
        ));
    }
    Ok(())
}

fn target_sessions_missing_explicit_models(
    config: &Config,
    sessions: &HashMap<String, Session>,
    targets: &[String],
) -> Vec<String> {
    targets
        .iter()
        .filter(|target| {
            sessions
                .get(*target)
                .is_none_or(|session| !session.effective_model_configured(config))
        })
        .cloned()
        .collect()
}

fn prune_direct_runs_locked(runs: &mut HashMap<String, DirectRunEntry>) {
    let cutoff = now_epoch().saturating_sub(DIRECT_RUN_RETAIN_SECS);
    runs.retain(|_, run| run.status.is_active() || run.updated_at >= cutoff);
}

fn with_direct_runs<R>(f: impl FnOnce(&mut HashMap<String, DirectRunEntry>) -> R) -> R {
    let mut guard = DIRECT_RUNS.lock().unwrap_or_else(|poisoned| {
        eprintln!("ERROR: session_control direct run registry mutex poisoned; recovering");
        poisoned.into_inner()
    });
    prune_direct_runs_locked(&mut guard);
    f(&mut guard)
}

fn register_direct_run(run_id: &str, session_id: &str, control: &DelegatedRunControl) {
    with_direct_runs(|runs| {
        runs.insert(
            run_id.to_string(),
            DirectRunEntry {
                session_id: session_id.to_string(),
                status: DirectRunStatus::Queued,
                cancel: control.cancel.clone(),
                stop_requested: Arc::clone(&control.stop_requested),
                updated_at: now_epoch(),
            },
        );
    });
}

fn with_group_run_controls<R>(
    f: impl FnOnce(&mut HashMap<String, GroupRunControlEntry>) -> R,
) -> R {
    let mut guard = GROUP_RUN_CONTROLS.lock().unwrap_or_else(|poisoned| {
        eprintln!("ERROR: session_control group run controls mutex poisoned; recovering");
        poisoned.into_inner()
    });
    f(&mut guard)
}

fn register_group_run_control(
    run_id: &str,
    group_id: &str,
    session_id: &str,
    control: &DelegatedRunControl,
) {
    with_group_run_controls(|runs| {
        runs.insert(
            run_id.to_string(),
            GroupRunControlEntry {
                group_id: group_id.to_string(),
                session_id: session_id.to_string(),
                cancel: control.cancel.clone(),
                stop_requested: Arc::clone(&control.stop_requested),
            },
        );
    });
}

fn active_group_run_statuses_by_session() -> HashMap<String, String> {
    let controls = with_group_run_controls(|runs| {
        runs.iter()
            .map(|(run_id, control)| {
                (
                    run_id.clone(),
                    control.group_id.clone(),
                    control.session_id.clone(),
                )
            })
            .collect::<Vec<_>>()
    });
    let mut statuses = HashMap::new();
    let mut groups = HashMap::new();
    for (run_id, group_id, session_id) in controls {
        let group = groups
            .entry(group_id.clone())
            .or_insert_with(|| session_group::load_group_from_storage_result(&group_id));
        // A control entry exists, so a run was registered and is likely in-flight. If the
        // group cannot be read (transient IO / parse error / concurrent rename) or the run
        // is missing from the loaded group, report "unknown" rather than "running": the
        // delete gate (session_has_active_delegated_work) treats "unknown" as active so a
        // legitimate delete is still refused on the safe side, while the session list
        // (session_runtime_status_from_snapshots / merge_group_session_status) treats any
        // non queued/running status as inactive, so the session is not shown falsely busy.
        let status = match group {
            Ok(Some(group)) => group
                .runs
                .iter()
                .find(|run| run.id == run_id)
                .map(|run| run.status.as_str())
                .unwrap_or("unknown"),
            Ok(None) | Err(_) => "unknown",
        };
        merge_group_session_status(&mut statuses, session_id, status.to_string(), now_epoch());
    }
    statuses
        .into_iter()
        .map(|(session_id, (status, _))| (session_id, status))
        .collect()
}

fn saved_group_has_active_run_for_session(session_id: &str) -> bool {
    let Ok(summaries) = session_group::list_saved_group_summaries_result() else {
        return true;
    };
    for summary in summaries {
        let Ok(group) = session_group::load_group_from_storage_result(&summary.id) else {
            return true;
        };
        let Some(group) = group else {
            continue;
        };
        if group.runs.iter().any(|run| {
            run.session_id == session_id && session_group::is_active_group_run_status(&run.status)
        }) {
            return true;
        }
    }
    false
}

fn clear_group_run_control(run_id: &str) {
    with_group_run_controls(|runs| {
        runs.remove(run_id);
    });
}

fn stop_group_run_controls(group_id: &str, run_ids: &[String]) -> usize {
    let run_id_set = run_ids.iter().collect::<HashSet<_>>();
    with_group_run_controls(|runs| {
        let mut stopped = 0usize;
        runs.retain(|run_id, run| {
            if run.group_id == group_id && run_id_set.contains(run_id) {
                run.stop_requested.store(true, Ordering::Relaxed);
                run.cancel.cancel();
                stopped += 1;
                false
            } else {
                true
            }
        });
        stopped
    })
}

fn update_direct_run_status(run_id: &str, status: DirectRunStatus) -> bool {
    with_direct_runs(|runs| {
        if let Some(run) = runs.get_mut(run_id) {
            if run.status.is_terminal() && run.status != status {
                return false;
            }
            run.status = status;
            run.updated_at = now_epoch();
            true
        } else {
            false
        }
    })
}

fn direct_run_status(run_id: &str) -> DirectRunStatus {
    with_direct_runs(|runs| {
        runs.get(run_id)
            .map(|run| run.status)
            .unwrap_or(DirectRunStatus::Failed)
    })
}

fn direct_run_is_active(run_id: &str) -> bool {
    direct_run_status(run_id).is_active()
}

fn target_set_contains_session(target_set: &HashSet<String>, session_id: &str) -> bool {
    target_set.contains(session_id)
        || target_set
            .iter()
            .any(|target| crate::session_ids_match(target, session_id))
}

fn stop_direct_runs_for_targets(targets: &[String]) -> usize {
    let target_set = targets.iter().cloned().collect::<HashSet<_>>();
    with_direct_runs(|runs| {
        let mut stopped = 0usize;
        for run in runs.values_mut() {
            if !run.status.is_active() || !target_set_contains_session(&target_set, &run.session_id)
            {
                continue;
            }
            run.status = DirectRunStatus::Stopped;
            run.updated_at = now_epoch();
            run.stop_requested.store(true, Ordering::Relaxed);
            run.cancel.cancel();
            stopped += 1;
        }
        stopped
    })
}

#[cfg(not(test))]
pub(crate) fn cancel_all_active_runs_for_storage() -> usize {
    let group_runs = with_group_run_controls(|runs| {
        let mut stopped = 0usize;
        for run in runs.values() {
            run.cancel.cancel();
            stopped += 1;
        }
        stopped
    });
    let direct_runs = with_direct_runs(|runs| {
        let mut stopped = 0usize;
        for run in runs.values_mut() {
            if !run.status.is_active() {
                continue;
            }
            run.status = DirectRunStatus::Stopped;
            run.updated_at = now_epoch();
            run.cancel.cancel();
            stopped += 1;
        }
        stopped
    });
    group_runs + direct_runs
}

fn direct_runs_for_session(session_id: &str) -> Vec<(String, DirectRunStatus, u64)> {
    with_direct_runs(|runs| {
        let mut out = runs
            .iter()
            .filter(|(_, run)| run.session_id == session_id)
            .map(|(run_id, run)| (run_id.clone(), run.status, run.updated_at))
            .collect::<Vec<_>>();
        out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        out
    })
}

fn direct_run_status_for_session(session_id: &str) -> Option<DirectRunStatus> {
    let runs = direct_runs_for_session(session_id);
    if runs
        .iter()
        .any(|(_, status, _)| matches!(status, DirectRunStatus::Running))
    {
        return Some(DirectRunStatus::Running);
    }
    if runs
        .iter()
        .any(|(_, status, _)| matches!(status, DirectRunStatus::Queued))
    {
        return Some(DirectRunStatus::Queued);
    }
    runs.first().map(|(_, status, _)| *status)
}

fn direct_run_statuses_by_session() -> HashMap<String, DirectRunStatus> {
    let mut latest: HashMap<String, (DirectRunStatus, u64)> = HashMap::new();
    with_direct_runs(|runs| {
        for run in runs.values() {
            merge_direct_session_status(
                &mut latest,
                run.session_id.clone(),
                run.status,
                run.updated_at,
            );
        }
    });
    latest
        .into_iter()
        .map(|(session_id, (status, _))| (session_id, status))
        .collect()
}

fn merge_direct_session_status(
    statuses: &mut HashMap<String, (DirectRunStatus, u64)>,
    session_id: String,
    status: DirectRunStatus,
    updated_at: u64,
) {
    let incoming_active = status.is_active();
    let replace = statuses
        .get(&session_id)
        .is_none_or(|(current, current_updated_at)| {
            let current_active = current.is_active();
            match (current_active, incoming_active) {
                (true, false) => false,
                (false, true) => true,
                (true, true)
                    if *current == DirectRunStatus::Queued
                        && status == DirectRunStatus::Running =>
                {
                    true
                }
                (true, true)
                    if *current == DirectRunStatus::Running
                        && status == DirectRunStatus::Queued =>
                {
                    false
                }
                _ => updated_at > *current_updated_at,
            }
        });
    if replace {
        statuses.insert(session_id, (status, updated_at));
    }
}

fn all_groups_for_session(session_id: &str) -> Result<Vec<SessionGroup>, String> {
    let mut groups = Vec::new();
    for summary in session_group::list_saved_group_summaries_result()
        .map_err(|_| storage_protected_control_error())?
    {
        if summary.corrupt {
            continue;
        }
        let Some(group) = session_group::load_group_from_storage_result(&summary.id)
            .map_err(|_| storage_protected_control_error())?
        else {
            continue;
        };
        let is_member = group.members.iter().any(|member| member == session_id);
        let has_run = group.runs.iter().any(|run| run.session_id == session_id);
        if is_member || has_run {
            groups.push(group);
        }
    }
    groups.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(groups)
}

fn group_run_status_from_groups(session_id: &str, groups: &[SessionGroup]) -> Option<String> {
    let mut latest: HashMap<String, (String, u64)> = HashMap::new();
    for group in groups {
        for run in group.runs.iter().filter(|run| run.session_id == session_id) {
            merge_group_session_status(
                &mut latest,
                run.session_id.clone(),
                run.status.clone(),
                run.updated_at,
            );
        }
    }
    latest
        .remove(session_id)
        .map(|(status, _updated_at)| status)
}

fn merge_group_session_status(
    statuses: &mut HashMap<String, (String, u64)>,
    session_id: String,
    status: String,
    updated_at: u64,
) {
    let incoming_active = matches!(status.as_str(), "running" | "queued");
    let replace = statuses
        .get(&session_id)
        .is_none_or(|(current, current_updated_at)| {
            let current_active = matches!(current.as_str(), "running" | "queued");
            match (current_active, incoming_active) {
                (true, false) => false,
                (false, true) => true,
                (true, true) if current == "queued" && status == "running" => true,
                (true, true) if current == "running" && status == "queued" => false,
                _ => updated_at > *current_updated_at,
            }
        });
    if replace {
        statuses.insert(session_id, (status, updated_at));
    }
}

async fn session_runtime_status(
    state: &AppState,
    session_id: &str,
    group_status: Option<&str>,
) -> String {
    let active_session_ids = {
        let active_runs = state.active_runs.lock().await;
        active_runs.keys().cloned().collect::<HashSet<_>>()
    };
    session_runtime_status_from_snapshots(
        &active_session_ids,
        session_id,
        direct_run_status_for_session(session_id),
        group_status,
    )
}

fn session_runtime_status_from_snapshots(
    active_session_ids: &HashSet<String>,
    session_id: &str,
    direct_status: Option<DirectRunStatus>,
    group_status: Option<&str>,
) -> String {
    if active_session_ids.contains(session_id) {
        return "running".to_string();
    }
    if let Some(status) = direct_status {
        if status == DirectRunStatus::Queued {
            return "queued".to_string();
        }
        if status == DirectRunStatus::Running {
            return "running".to_string();
        }
    }
    if let Some(status) = group_status
        && matches!(status, "queued" | "running")
    {
        return status.to_string();
    }
    "idle".to_string()
}

#[derive(Clone, Debug, Default)]
struct ProfileFieldSummary {
    text: String,
    template_unfilled: bool,
}

#[derive(Clone, Debug, Default)]
struct SessionProfileSummary {
    agent: ProfileFieldSummary,
    identity: ProfileFieldSummary,
    user: ProfileFieldSummary,
    style: ProfileFieldSummary,
}

fn limit_summary_text(value: &str, max_chars: usize) -> String {
    let mut compact = value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    crate::truncate_safe(&mut compact, max_chars);
    compact
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &Metadata) -> bool {
    false
}

fn metadata_is_link_like(metadata: &Metadata) -> bool {
    metadata.file_type().is_symlink() || metadata_is_reparse_point(metadata)
}

fn normalize_profile_secret_key(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn is_profile_secret_key(value: &str) -> bool {
    let normalized = normalize_profile_secret_key(value);
    normalized == "authorization"
        || normalized.ends_with("apikey")
        || normalized.ends_with("token")
        || normalized.ends_with("accesstoken")
        || normalized.ends_with("password")
        || normalized.ends_with("passwd")
        || normalized.ends_with("secret")
        || normalized.ends_with("credential")
        || normalized.ends_with("accesskey")
        || normalized.ends_with("accesskeyid")
        || normalized.ends_with("privatekey")
        || normalized.ends_with("header")
        || normalized.ends_with("headers")
}

fn looks_like_profile_bare_secret(value: &str) -> bool {
    let trimmed =
        value.trim_matches(|ch: char| ch.is_ascii_punctuation() && !matches!(ch, '_' | '-' | '.'));
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    [
        "sk-",
        "ghp_",
        "github_pat_",
        "hf_",
        "xox",
        "ya29.",
        "eyj",
        "akia",
        "asia",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn redact_profile_token(token: &str) -> String {
    if looks_like_profile_bare_secret(token) {
        return "[redacted]".to_string();
    }
    if let Some((key, _value)) = token.split_once('=')
        && is_profile_secret_key(key)
    {
        return format!("{key}=[redacted]");
    }
    token.to_string()
}

fn profile_secret_key_from_label(token: &str) -> Option<String> {
    let label = token.trim_start_matches(['-', '*']).trim();
    let key = label.trim_end_matches(':').trim();
    if key.is_empty() || !is_profile_secret_key(key) {
        return None;
    }
    let normalized = normalize_profile_secret_key(key);
    if matches!(normalized.as_str(), "header" | "headers") && !label.ends_with(':') {
        return None;
    }
    Some(if label.ends_with(':') {
        label.to_string()
    } else {
        format!("{key}:")
    })
}

fn profile_secret_label_from_tokens(current: &str, next: Option<&str>) -> Option<(String, usize)> {
    if let Some(label) = profile_secret_key_from_label(current) {
        return Some((label, 1));
    }
    let next = next?;
    if is_profile_secret_compound_label(current, next) {
        return Some((format!("{current} {next}:"), 2));
    }
    None
}

fn is_profile_secret_compound_label(current: &str, next: &str) -> bool {
    let combined = format!(
        "{}{}",
        normalize_profile_secret_key(current),
        normalize_profile_secret_key(next)
    );
    matches!(
        combined.as_str(),
        "apikey" | "accesstoken" | "accesskey" | "accesskeyid" | "privatekey"
    )
}

fn is_profile_secret_connector(token: &str) -> bool {
    let trimmed = token.trim_matches(|ch: char| ch.is_ascii_punctuation());
    trimmed.eq_ignore_ascii_case("is")
        || trimmed.eq_ignore_ascii_case("as")
        || trimmed.eq_ignore_ascii_case("equals")
        || trimmed.eq_ignore_ascii_case("value")
        || matches!(token.trim(), "=" | ":" | "=>")
}

fn redact_profile_summary_text(value: &str) -> String {
    let trimmed = value.trim();
    if let Ok(json_value) = serde_json::from_str::<Value>(trimmed) {
        return serde_json::to_string(&redact_profile_json_value(&json_value))
            .unwrap_or_else(|_| "[redacted]".to_string());
    }
    redact_profile_plain_text(trimmed)
}

fn redact_profile_plain_text(value: &str) -> String {
    let trimmed = value.trim();
    if let Some((key, _value)) = trimmed.split_once(':')
        && is_profile_secret_key(key.trim_start_matches('-').trim())
    {
        return format!("{}: [redacted]", key.trim());
    }
    let tokens = trimmed.split_whitespace().collect::<Vec<_>>();
    let mut redacted = Vec::with_capacity(tokens.len());
    let mut i = 0usize;
    while i < tokens.len() {
        let token = tokens[i];
        let normalized =
            normalize_profile_secret_key(token.trim_matches(|ch: char| {
                ch.is_ascii_punctuation() && !matches!(ch, '_' | '-' | '.')
            }));
        if normalized == "bearer" {
            redacted.push("Bearer".to_string());
            if i + 1 < tokens.len() {
                redacted.push("[redacted]".to_string());
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if let Some((label, consumed_label_tokens)) =
            profile_secret_label_from_tokens(token, tokens.get(i + 1).copied())
        {
            let is_authorization = normalize_profile_secret_key(&label) == "authorization";
            redacted.push(label);
            let mut value_index = i + consumed_label_tokens;
            if tokens
                .get(value_index)
                .is_some_and(|token| is_profile_secret_connector(token))
            {
                value_index += 1;
            }
            if value_index < tokens.len() {
                redacted.push("[redacted]".to_string());
                if is_authorization && value_index + 1 < tokens.len() {
                    i = value_index + 2;
                } else {
                    i = value_index + 1;
                }
            } else {
                i += consumed_label_tokens;
            }
            continue;
        }
        redacted.push(redact_profile_token(token));
        i += 1;
    }
    redact_profile_quoted_secret_values(&redacted.join(" "))
}

fn redact_profile_json_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    if is_profile_secret_key(key) {
                        (key.clone(), Value::String("[redacted]".to_string()))
                    } else {
                        (key.clone(), redact_profile_json_value(value))
                    }
                })
                .collect(),
        ),
        Value::Array(values) => {
            Value::Array(values.iter().map(redact_profile_json_value).collect())
        }
        Value::String(text) => {
            if looks_like_profile_bare_secret(text) {
                Value::String("[redacted]".to_string())
            } else {
                Value::String(redact_profile_plain_text(text))
            }
        }
        _ => value.clone(),
    }
}

fn redact_profile_quoted_secret_values(value: &str) -> String {
    static QUOTED_SECRET_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r#"(?i)(["']?)(authorization|api[_-]?key|access[_-]?token|token|password|passwd|secret|credential|access[_-]?key[_-]?id|private[_-]?key)(["']?\s*[:=]\s*["'])([^"']*)(["'])"#,
        )
        .expect("quoted secret regex should compile")
    });
    static UNQUOTED_SECRET_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r#"(?i)(\b)(authorization|api[_-]?key|access[_-]?token|token|password|passwd|secret|credential|access[_-]?key[_-]?id|private[_-]?key)(\s*[:=]\s*)([^\s,}\[\]\)>]+)"#,
        )
        .expect("unquoted secret regex should compile")
    });
    let value = QUOTED_SECRET_RE
        .replace_all(value, "$1$2$3[redacted]$5")
        .into_owned();
    UNQUOTED_SECRET_RE
        .replace_all(&value, "$1$2$3[redacted]")
        .into_owned()
}

fn unquote_yaml_scalar(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

fn frontmatter_summary(content: &str) -> Option<String> {
    let normalized = content.strip_prefix('\u{feff}').unwrap_or(content);
    let normalized = normalized.replace("\r\n", "\n");
    let mut lines = normalized.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for line in lines {
        if line.trim() == "---" {
            return None;
        }
        let trimmed = line.trim();
        if let Some(value) = trimmed.strip_prefix("summary:") {
            let summary = unquote_yaml_scalar(value);
            let summary = limit_summary_text(&redact_profile_summary_text(&summary), 220);
            if !summary.is_empty() {
                return Some(summary);
            }
        }
    }
    None
}

fn strip_frontmatter(content: &str) -> &str {
    let normalized = content.strip_prefix('\u{feff}').unwrap_or(content);
    if !normalized.starts_with("---") {
        return normalized;
    }
    let mut lines = normalized.lines();
    if lines.next().map(str::trim) != Some("---") {
        return normalized;
    }
    let first_line_end = normalized.find('\n').map(|idx| idx + 1).unwrap_or(0);
    if first_line_end == 0 {
        return normalized;
    }
    let rest = &normalized[first_line_end..];
    let mut rest_offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let line_content = line.trim_end_matches(['\r', '\n']);
        if line_content.trim() == "---" {
            let consumed = first_line_end + rest_offset + line.len();
            return normalized[consumed..].trim_start();
        }
        rest_offset += line.len();
    }
    normalized
}

fn strip_markdown_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('*')
        .trim()
        .trim_matches('_')
        .trim()
        .to_string()
}

fn is_placeholder_value(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.is_empty()
        || trimmed == "-"
        || trimmed.eq_ignore_ascii_case("n/a")
        || trimmed.starts_with("_(")
        || trimmed.eq_ignore_ascii_case("TODO")
        || trimmed.eq_ignore_ascii_case("TODO:")
}

fn extract_markdown_field(content: &str, field: &str) -> Option<String> {
    let field_lower = field.to_ascii_lowercase();
    for line in strip_frontmatter(content).lines() {
        let trimmed = line
            .trim()
            .trim_start_matches('-')
            .trim()
            .trim_start_matches('*')
            .trim();
        let lowered = trimmed.to_ascii_lowercase();
        let patterns = [format!("**{}:**", field_lower), format!("{}:", field_lower)];
        for pattern in patterns {
            if lowered.starts_with(&pattern) {
                let raw_value = trimmed[pattern.len()..].trim();
                let value = strip_markdown_value(raw_value);
                if !is_placeholder_value(&value) {
                    return Some(limit_summary_text(
                        &redact_profile_summary_text(&value),
                        180,
                    ));
                }
            }
        }
    }
    None
}

fn collect_section_bullets(content: &str, section: &str, max_items: usize) -> Vec<String> {
    let target = format!("## {}", section).to_ascii_lowercase();
    let mut in_section = false;
    let mut items = Vec::new();
    for line in strip_frontmatter(content).lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            in_section = trimmed.to_ascii_lowercase() == target;
            continue;
        }
        if !in_section || !trimmed.starts_with('-') {
            continue;
        }
        let item = strip_markdown_value(trimmed.trim_start_matches('-'));
        if !is_placeholder_value(&item) {
            items.push(limit_summary_text(&redact_profile_summary_text(&item), 160));
            if items.len() >= max_items {
                break;
            }
        }
    }
    items
}

fn fallback_markdown_summary(content: &str) -> Option<String> {
    let body = strip_frontmatter(content);
    let mut parts = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed == "---"
            || trimmed.starts_with("<!--")
            || trimmed.starts_with('#')
        {
            continue;
        }
        if trimmed.starts_with('-') {
            let item = strip_markdown_value(trimmed.trim_start_matches('-'));
            if !is_placeholder_value(&item) {
                parts.push(redact_profile_summary_text(&item));
            }
        } else {
            parts.push(redact_profile_summary_text(trimmed));
        }
        if parts.len() >= 3 {
            break;
        }
    }
    let summary = limit_summary_text(&parts.join(" "), 220);
    if summary.is_empty() {
        None
    } else {
        Some(summary)
    }
}

fn file_summary_from_content(content: &str, fields: &[&str], sections: &[&str]) -> String {
    if let Some(summary) = frontmatter_summary(content) {
        return summary;
    }
    let mut parts = Vec::new();
    for field in fields {
        if let Some(value) = extract_markdown_field(content, field) {
            parts.push(format!("{field}: {value}"));
        }
    }
    for section in sections {
        let bullets = collect_section_bullets(content, section, 2);
        if !bullets.is_empty() {
            parts.push(format!("{section}: {}", bullets.join("; ")));
        }
    }
    let summary = limit_summary_text(&parts.join("; "), 220);
    if !summary.is_empty() {
        return summary;
    }
    fallback_markdown_summary(content).unwrap_or_default()
}

fn read_workspace_file(workspace: &Path, names: &[&str]) -> Option<(String, String)> {
    let workspace_root = workspace.canonicalize().ok();
    for name in names {
        let path = workspace.join(name);
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata_is_link_like(&metadata) || !metadata.is_file() {
            continue;
        }
        if let (Some(workspace_root), Ok(canonical_path)) = (&workspace_root, path.canonicalize())
            && !canonical_path.starts_with(workspace_root)
        {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !content.trim().is_empty() {
            return Some(((*name).to_string(), content));
        }
    }
    None
}

fn identity_template_unfilled(content: &str) -> bool {
    content.contains("# IDENTITY.md - Agent Profile")
        && extract_markdown_field(content, "Name").is_none()
        && extract_markdown_field(content, "Role").is_none()
        && extract_markdown_field(content, "Style").is_none()
}

fn user_template_unfilled(content: &str) -> bool {
    content.contains("# USER.md - User Profile")
        && extract_markdown_field(content, "Name").is_none()
        && extract_markdown_field(content, "Preferred address").is_none()
        && extract_markdown_field(content, "Timezone").is_none()
        && collect_section_bullets(content, "Preferences", 1).is_empty()
        && collect_section_bullets(content, "Context", 1).is_empty()
}

fn agent_template_unfilled(content: &str) -> bool {
    content.contains("# AGENTS.md - Main Agent Rules")
        && !content.contains("## Session Control Profile")
}

fn summarize_session_profile(workspace: &Path) -> SessionProfileSummary {
    let mut summary = SessionProfileSummary::default();
    if let Some((_name, content)) = read_workspace_file(workspace, &["AGENTS.md", "AGENT.md"]) {
        summary.agent = ProfileFieldSummary {
            text: file_summary_from_content(&content, &[], &["Session Control Profile"]),
            template_unfilled: agent_template_unfilled(&content),
        };
    }
    if let Some((_name, content)) = read_workspace_file(workspace, &["IDENTITY.md"]) {
        summary.identity = ProfileFieldSummary {
            text: file_summary_from_content(&content, &["Name", "Role", "Style"], &[]),
            template_unfilled: identity_template_unfilled(&content),
        };
    }
    if let Some((_name, content)) = read_workspace_file(workspace, &["USER.md"]) {
        summary.user = ProfileFieldSummary {
            text: file_summary_from_content(
                &content,
                &["Name", "Preferred address", "Timezone"],
                &["Preferences", "Context"],
            ),
            template_unfilled: user_template_unfilled(&content),
        };
    }
    if let Some((_name, content)) = read_workspace_file(workspace, &["SOUL.md"]) {
        summary.style = ProfileFieldSummary {
            text: file_summary_from_content(
                &content,
                &[],
                &["Defaults", "Execution", "Boundaries"],
            ),
            template_unfilled: false,
        };
    }
    summary
}

fn enabled_skills_for_session(session: &Session) -> Vec<prompts::SkillMeta> {
    enabled_skills_for_workspace(&session.workspace, &session.enabled_system_skills)
}

fn enabled_skills_for_workspace(
    workspace: &Path,
    enabled_system_skills: &HashSet<String>,
) -> Vec<prompts::SkillMeta> {
    prompts::discover_all_skills(workspace)
        .into_iter()
        .filter(|skill| {
            skill.source != SkillSource::System
                || prompts::is_system_skill_enabled(&skill.path, enabled_system_skills)
        })
        .collect()
}

fn enabled_mcp_tools_for_session(
    config: &crate::Config,
    session: &Session,
) -> Vec<tools::mcp::McpToolDescriptor> {
    enabled_mcp_tools_for_workspace(config, &session.workspace)
}

fn enabled_mcp_tools_for_workspace(
    config: &crate::Config,
    workspace: &Path,
) -> Vec<tools::mcp::McpToolDescriptor> {
    let policy = tools::mcp::load_session_policy(workspace);
    tools::mcp::cached_list_tools_for_policy(config, workspace, &policy)
}

fn session_summary_from_profile(profile: &SessionProfileSummary) -> (String, String) {
    let agent = if profile.agent.text.is_empty() {
        "none".to_string()
    } else {
        profile.agent.text.clone()
    };
    let user = if profile.user.text.is_empty() {
        "none".to_string()
    } else {
        profile.user.text.clone()
    };
    (agent, user)
}

fn format_profile_line(name: &str, field: &ProfileFieldSummary) -> String {
    if field.text.is_empty() {
        format!("- {name}: none")
    } else if field.template_unfilled {
        format!("- {name}: {} (template_unfilled=true)", field.text)
    } else {
        format!("- {name}: {}", field.text)
    }
}

fn protocol_mention_token(token: &str, member_set: &HashSet<&str>) -> Option<String> {
    let token = token.trim_matches(|ch: char| {
        !ch.is_ascii_alphanumeric() && !matches!(ch, '@' | '-' | '_' | '.')
    });
    let raw = token.strip_prefix('@')?;
    if raw.is_empty()
        || !raw
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return None;
    }
    if member_set.contains(raw) {
        return Some(raw.to_string());
    }
    let without_sentence_dot = raw.trim_end_matches('.');
    if without_sentence_dot != raw && member_set.contains(without_sentence_dot) {
        return Some(without_sentence_dot.to_string());
    }
    None
}

fn mentions_from_text(text: &str, members: &[String]) -> Vec<String> {
    let member_set = members.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        if let Some(candidate) = protocol_mention_token(token, &member_set)
            && seen.insert(candidate.clone())
        {
            out.push(candidate);
        }
    }
    out
}

fn text_mentions_all(text: &str) -> bool {
    text.split_whitespace().any(|token| {
        token
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '@')
            .eq_ignore_ascii_case("@all")
    })
}

async fn mutate_group<F, R>(group_id: &str, f: F) -> Result<(SessionGroup, R), String>
where
    F: FnOnce(&mut SessionGroup) -> R,
{
    mutate_group_result(group_id, |group| Ok::<R, String>(f(group))).await
}

async fn mutate_group_result<F, R>(group_id: &str, f: F) -> Result<(SessionGroup, R), String>
where
    F: FnOnce(&mut SessionGroup) -> Result<R, String>,
{
    let group_id = session_group::validate_group_id(group_id)?.to_string();
    let gate = session_group::group_persist_gate(&group_id);
    let _guard = gate.lock().await;
    let mut group = session_group::load_group_from_storage_result(&group_id)?
        .ok_or_else(|| format!("Group '{}' not found", group_id))?;
    let result = f(&mut group)?;
    group.updated_at = now_epoch();
    session_group::save_group_to_disk_locked(&group).await?;
    Ok((group, result))
}

fn append_group_message(
    group: &mut SessionGroup,
    role: &str,
    session_id: Option<String>,
    content: String,
    turn_id: Option<String>,
    run_id: Option<String>,
) -> GroupMessage {
    let message = GroupMessage {
        id: next_id("gmsg"),
        role: role.to_string(),
        session_id,
        content,
        timestamp: now_epoch(),
        turn_id,
        run_id,
    };
    group.messages.push(message.clone());
    message
}

fn run_status_event(group_id: &str, run: &GroupRun) -> Value {
    json!({
        "type": "group_member_status",
        "group_id": group_id,
        "run_id": run.id,
        "session_id": run.session_id,
        "status": run.status,
        "error": run.error,
        "result_excerpt": run.result_excerpt,
        "updated_at": run.updated_at,
    })
}

fn apply_group_run_status_transition(
    run: &mut GroupRun,
    status: &str,
    result_excerpt: Option<String>,
    error: Option<String>,
    now: u64,
) -> Option<GroupRun> {
    match status {
        "running" => {
            if run.status != "queued" {
                return None;
            }
            run.status = "running".to_string();
            run.updated_at = now;
        }
        "completed" | "failed" | "stopped" => {
            if !session_group::is_active_group_run_status(&run.status) {
                return None;
            }
            run.status = status.to_string();
            if let Some(result_excerpt) = result_excerpt {
                run.result_excerpt = Some(result_excerpt);
            }
            if let Some(error) = error {
                run.error = Some(error);
            }
            run.updated_at = now;
            run.completed_at = Some(now);
        }
        _ => return None,
    }
    Some(run.clone())
}

async fn emit_group_run_status_events(state: &AppState, group_id: &str, run: &GroupRun) {
    crate::send_group_client_event(
        state,
        group_id,
        GroupClientEvent::reliable(run_status_event(group_id, run)),
    )
    .await;
    if matches!(run.status.as_str(), "completed" | "failed" | "stopped") {
        crate::send_group_client_event(
            state,
            group_id,
            GroupClientEvent::reliable(json!({
                "type": "group_run_completed",
                "group_id": group_id,
                "run_id": run.id,
                "session_id": run.session_id,
                "status": run.status,
                "result_excerpt": run.result_excerpt,
                "error": run.error,
                "completed_at": run.completed_at,
                "updated_at": run.updated_at,
            })),
        )
        .await;
    }
}

async fn update_run_status(
    state: &AppState,
    group_id: &str,
    run_id: &str,
    status: &str,
    result_excerpt: Option<String>,
    error: Option<String>,
) -> Result<Option<GroupRun>, String> {
    let result_excerpt = result_excerpt.map(|text| redact_profile_summary_text(&text));
    let error = error.map(|text| redact_profile_summary_text(&text));
    let group_id = session_group::validate_group_id(group_id)?.to_string();
    let updated = {
        let gate = session_group::group_persist_gate(&group_id);
        let _guard = gate.lock().await;
        let mut group = session_group::load_group_from_storage_result(&group_id)?
            .ok_or_else(|| format!("Group '{}' not found", group_id))?;
        let now = now_epoch();
        let updated = group
            .runs
            .iter_mut()
            .find(|run| run.id == run_id)
            .and_then(|run| {
                apply_group_run_status_transition(
                    run,
                    status,
                    result_excerpt.clone(),
                    error.clone(),
                    now,
                )
            });
        if updated.is_none() {
            return Ok(None);
        }
        group.updated_at = now;
        session_group::save_group_to_disk_locked(&group).await?;
        updated
    };

    if let Some(run) = updated.as_ref() {
        emit_group_run_status_events(state, &group_id, run).await;
    }
    Ok(updated)
}

async fn session_control_lock(state: &AppState, session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = state.session_control_locks.lock().await;
    locks
        .entry(session_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

async fn wait_until_session_idle(state: &AppState, session_id: &str, cancel: &CancellationToken) {
    loop {
        if cancel.is_cancelled() {
            return;
        }
        let busy = {
            let runs = state.active_runs.lock().await;
            runs.contains_key(session_id)
        };
        if !busy {
            return;
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return,
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
    }
}

async fn append_target_session_message(
    state: &Arc<AppState>,
    session_id: &str,
    message: String,
) -> Result<usize, String> {
    runtime_loop::ensure_session_ready(state, Some(session_id)).await?;
    let message_index = {
        let mut sessions = state.sessions.lock().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| format!("Session '{}' not found", session_id))?;
        if let Some(plan) = session
            .pending_plan
            .as_ref()
            .filter(|plan| plan.status.is_active())
        {
            return Err(format!(
                "Session '{}' has active plan '{}' revision {}; execute or discard it before dispatching new work",
                session_id, plan.id, plan.revision
            ));
        }
        let message_index = session.messages.len();
        session.messages.push(ChatMessage {
            role: "user".to_string(),
            content: Some(message),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(now_epoch()),
        });
        if !session.pending_plan.as_ref().is_some_and(|plan| {
            matches!(
                plan.status,
                crate::plan::PlanStatus::Failed | crate::plan::PlanStatus::Stopped
            )
        }) {
            session.pending_plan = None;
        }
        session.updated_at = now_epoch();
        message_index
    };
    session_store::save_current_session_to_disk(state, session_id).await?;
    Ok(message_index)
}

fn target_prompt(
    group_id: Option<&str>,
    source_message: &str,
    group_context: Option<&str>,
    optional_reply: bool,
) -> String {
    match group_id {
        Some(group_id) => {
            let reply_policy = if optional_reply {
                "Your reply is optional. If you have no useful contribution, respond exactly `NO_REPLY` and do not add extra text."
            } else {
                "You must reply with one useful group contribution."
            };
            format!(
                "[Session group: {group_id}]\n\
                 Main session asked this session to contribute one response to the group conversation.\n\
                 {reply_policy}\n\
                 Respond in your own session using your normal tools and permissions. Do not assume other sessions can see private tool outputs unless you summarize them.\n\n\
                 Group context summary:\n{}\n\n\
                 Main instruction:\n{source_message}",
                group_context.unwrap_or("No group context is available.")
            )
        }
        None => format!(
            "[Main session delegation]\n\
             Main session asked this session to complete this delegated task. Respond in your own session using your normal tools and permissions.\n\n\
             Main instruction:\n{source_message}"
        ),
    }
}

fn target_group_context(group_id: &str, budget: usize) -> Result<String, String> {
    let mut context = session_group::load_group_from_storage_result(group_id)?
        .map(|group| collect_group_summary(&group))
        .unwrap_or_else(|| "No group context is available.".to_string());
    crate::truncate_safe(&mut context, budget.clamp(500, 8_000));
    Ok(context)
}

fn stored_group_run_prompt(message: &str) -> String {
    message.to_string()
}

async fn latest_assistant_excerpt_after(
    state: &AppState,
    session_id: &str,
    boundary_user_message: &str,
    budget: usize,
) -> Option<String> {
    let sessions = state.sessions.lock().await;
    let session = sessions.get(session_id)?;
    latest_assistant_content_after(&session.messages, boundary_user_message, budget)
}

/// True for the auto-generated compression summary assistant message, which is
/// not a genuine reply and must never be returned as this run's answer.
fn is_auto_compress_summary(message: &ChatMessage) -> bool {
    message
        .content
        .as_deref()
        .is_some_and(|content| content.starts_with("## Context Summary (auto-generated)"))
}

/// Return the text of the last assistant turn produced AFTER the delegated user
/// message identified by `boundary_user_message`.
///
/// Uses the appended message's identity (not an absolute index) so it stays
/// correct after `AutoCompressContextHook` shrinks/re-indexes `messages`
/// mid-run. If that boundary message survived compression we scan strictly
/// after it; if it was folded into the auto-summary, `build_compressed_messages`
/// guarantees the entire kept tail is from this run, so we scan all messages.
/// Either way we take the last assistant message with non-empty content,
/// skipping the auto-summary and tool-call-only (content:None) turns.
fn latest_assistant_content_after(
    messages: &[ChatMessage],
    boundary_user_message: &str,
    budget: usize,
) -> Option<String> {
    let boundary = messages.iter().rposition(|message| {
        message.role == "user" && message.content.as_deref() == Some(boundary_user_message)
    });
    let start = boundary.map(|index| index + 1).unwrap_or(0);
    let mut content = messages
        .get(start..)?
        .iter()
        .rev()
        .find(|message| {
            message.role == "assistant"
                && message.has_nonempty_content()
                && !is_auto_compress_summary(message)
        })
        .and_then(|message| message.content.clone())?;
    crate::truncate_safe(&mut content, budget.max(1));
    Some(content)
}

async fn record_group_session_result(
    state: &AppState,
    group_id: &str,
    run_id: &str,
    session_id: &str,
    content: String,
) -> bool {
    if is_no_reply_result(&content) {
        return false;
    }
    let content = redact_profile_summary_text(&content);
    let message = mutate_group(group_id, |group| {
        append_group_message(
            group,
            "session",
            Some(session_id.to_string()),
            content,
            None,
            Some(run_id.to_string()),
        )
    })
    .await
    .map(|(_, message)| message);
    let message = match message {
        Ok(message) => message,
        Err(error) => {
            eprintln!("ERROR: failed to persist group session result: {error}");
            return false;
        }
    };
    crate::send_group_client_event(
        state,
        group_id,
        GroupClientEvent::reliable(json!({
            "type": "group_message",
            "group_id": group_id,
            "message": message,
        })),
    )
    .await;
    true
}

fn is_no_reply_result(content: &str) -> bool {
    if content.trim().is_empty() {
        return true;
    }
    // Reuse the shared fence stripper instead of re-implementing it here.
    let trimmed = crate::strip_json_fences(content);
    let normalized = trimmed.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '`' | '"'
                    | '\''
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '.'
                    | ','
                    | ';'
                    | ':'
                    | '!'
                    | '?'
            )
    });
    normalized.eq_ignore_ascii_case("NO_REPLY")
}

fn group_run_status_records_result(status: &str) -> bool {
    status == "completed"
}

fn group_mention_followup_prompt(source_session_id: &str, content: &str) -> String {
    let redacted_content = redact_profile_summary_text(content);
    format!(
        "[Group mention from {source_session_id}]\n{redacted_content}\n\nReply because this message mentioned your session id or used @all."
    )
}

async fn dispatch_group_mentions_from_session_result(
    state: &Arc<AppState>,
    group_id: &str,
    source_session_id: &str,
    content: &str,
    run_mode: AgentRunMode,
    summary_budget: usize,
    mention_depth: u8,
) {
    if mention_depth >= 1 || is_no_reply_result(content) {
        return;
    }
    let group = match session_group::load_group_from_storage_result(group_id) {
        Ok(Some(group)) => group,
        Ok(None) => return,
        Err(error) => {
            eprintln!("ERROR: failed to read group before mention follow-up: {error}");
            return;
        }
    };
    let is_admin = group.admins.iter().any(|admin| admin == source_session_id);
    let direct_mentions = mentions_from_text(content, &group.members)
        .into_iter()
        .filter(|target| target != source_session_id)
        .collect::<Vec<_>>();
    let mentions_all = is_admin && text_mentions_all(content);
    let mut targets = if mentions_all {
        group
            .members
            .iter()
            .filter(|member| member.as_str() != source_session_id)
            .cloned()
            .collect::<Vec<_>>()
    } else {
        direct_mentions.clone()
    };
    if !is_admin && targets.len() > 1 {
        targets.truncate(1);
    }
    targets.sort();
    targets.dedup();
    if targets.is_empty() {
        return;
    }
    let optional_targets = if mentions_all {
        let forced = direct_mentions.into_iter().collect::<HashSet<_>>();
        targets
            .iter()
            .filter(|target| !forced.contains(*target))
            .cloned()
            .collect()
    } else {
        HashSet::new()
    };
    let targets = match prepare_dispatch_targets(
        state,
        Some(group_id),
        targets,
        if mentions_all {
            SESSION_CONTROL_MEMBERS_MAX_ITEMS
        } else {
            SESSION_CONTROL_TARGETS_MAX_ITEMS
        },
    )
    .await
    {
        Ok(targets) => targets,
        Err(error) => {
            eprintln!("ERROR: failed to dispatch group mention follow-up: {error}");
            return;
        }
    };
    // Let each target reach run_target_run's final model gate. A missing or
    // invalid model is then recorded and broadcast as a visible failed run
    // instead of silently dropping the automatic mention follow-up here.
    let message = group_mention_followup_prompt(source_session_id, content);
    if let Err(error) = dispatch_to_sessions(
        state,
        DispatchRequest {
            group_id: Some(group_id.to_string()),
            targets,
            optional_targets,
            message,
            group_message: None,
            run_mode,
            wait: false,
            summary_budget,
            mention_depth: mention_depth.saturating_add(1),
        },
    )
    .await
    {
        eprintln!("ERROR: failed to dispatch group mention follow-up: {error}");
    }
}

async fn group_run_is_active(group_id: &str, run_id: &str) -> Result<bool, String> {
    Ok(session_group::load_group_from_storage_result(group_id)?
        .and_then(|group| group.runs.into_iter().find(|run| run.id == run_id))
        .map(|run| matches!(run.status.as_str(), "queued" | "running"))
        .unwrap_or(false))
}

/// A group websocket event plus whether it may be dropped under backpressure. Producers
/// declare droppability where the event's meaning is known (instead of the send path
/// guessing from the payload type), so a new high-frequency event can never silently cancel
/// slow sockets by being forgotten in a drop list.
pub(crate) struct GroupClientEvent {
    pub(crate) payload: Value,
    pub(crate) droppable: bool,
}

impl GroupClientEvent {
    /// Must reach the client; a full channel cancels the slow socket.
    pub(crate) fn reliable(payload: Value) -> Self {
        Self {
            payload,
            droppable: false,
        }
    }

    /// May be dropped when the channel is full because it is also recovered through live
    /// replay (member live events).
    pub(crate) fn droppable(payload: Value) -> Self {
        Self {
            payload,
            droppable: true,
        }
    }
}

async fn forward_group_live_event(
    state: &AppState,
    group_id: &str,
    run_id: &str,
    session_id: &str,
    event: Value,
) {
    // Member live events are high-frequency and recoverable via live replay, so they are
    // droppable under client backpressure rather than cancelling the slow socket.
    crate::send_group_client_event(
        state,
        group_id,
        GroupClientEvent::droppable(json!({
            "type": "group_member_event",
            "group_id": group_id,
            "run_id": run_id,
            "session_id": session_id,
            "event": event,
        })),
    )
    .await;
}

/// Resolve the id->display-name map for a group's members and the main owner, preferring
/// in-memory loaded sessions and only reading persisted session summaries for ids that are
/// not currently loaded. This keeps the common group broadcast path off the filesystem.
pub(crate) async fn group_member_name_map(
    state: &AppState,
    group: &SessionGroup,
) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let mut needs_disk_fallback = false;
    {
        let sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get(MAIN_SESSION_ID) {
            names.insert(MAIN_SESSION_ID.to_string(), session.name.clone());
        } else {
            needs_disk_fallback = true;
        }
        for member in &group.members {
            if let Some(session) = sessions.get(member) {
                names.insert(member.clone(), session.name.clone());
            } else {
                needs_disk_fallback = true;
            }
        }
    }
    if needs_disk_fallback {
        for (id, name) in session_group::saved_session_name_map() {
            names.entry(id).or_insert(name);
        }
    }
    names
}

#[derive(Default)]
struct GroupModelConfiguration {
    override_members: Vec<String>,
    configured_members: Vec<String>,
}

fn record_group_member_model_configuration(
    model_configuration: &mut GroupModelConfiguration,
    member: &str,
    session: &Session,
    config: &Config,
) {
    if session.has_explicit_model_override(config) {
        model_configuration
            .override_members
            .push(member.to_string());
    }
    if session.effective_model_configured(config) {
        model_configuration
            .configured_members
            .push(member.to_string());
    }
}

async fn group_model_configuration(
    state: &AppState,
    group: &SessionGroup,
    config: &Config,
) -> GroupModelConfiguration {
    let (mut model_configuration, unresolved) = {
        let sessions = state.sessions.lock().await;
        let mut model_configuration = GroupModelConfiguration::default();
        let mut unresolved = Vec::new();
        for member in &group.members {
            match sessions.get(member) {
                Some(session) => {
                    record_group_member_model_configuration(
                        &mut model_configuration,
                        member,
                        session,
                        config,
                    );
                }
                None => unresolved.push(member.clone()),
            }
        }
        (model_configuration, unresolved)
    };
    for member in unresolved {
        match session_store::load_session_from_storage_result(&member) {
            Ok(Some(session)) => {
                record_group_member_model_configuration(
                    &mut model_configuration,
                    &member,
                    &session,
                    config,
                );
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!(
                    "ERROR: failed to read group member model configuration for {member}: {error}"
                );
            }
        }
    }
    model_configuration
}

fn attach_group_model_configuration(
    payload: &mut Value,
    model_configuration: &GroupModelConfiguration,
    config: &Config,
    config_revision: u64,
) {
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "model_override_members".to_string(),
            json!(&model_configuration.override_members),
        );
        object.insert(
            "model_configured_members".to_string(),
            json!(&model_configuration.configured_members),
        );
        object.insert(
            "explicitPrimaryModelConfigured".to_string(),
            json!(config.explicit_primary_model_configured),
        );
        object.insert("configRevision".to_string(), json!(config_revision));
        object.insert(
            "capabilities".to_string(),
            json!({
                "s3": config.s3.is_some(),
                "s3_config_id": config.s3.as_ref().map(crate::image_uploads::s3_config_id),
            }),
        );
    }
}

async fn group_model_configuration_payload_with_config(
    state: &AppState,
    group: &SessionGroup,
    config: &Config,
    config_revision: u64,
) -> Value {
    let model_configuration = group_model_configuration(state, group, config).await;
    let mut payload = json!({
        "type": "group_model_configuration",
        "id": group.id,
        "model_member_ids": &group.members,
    });
    attach_group_model_configuration(&mut payload, &model_configuration, config, config_revision);
    payload
}

async fn group_json_with_config(
    state: &AppState,
    group: &SessionGroup,
    config: &Config,
    config_revision: u64,
) -> Value {
    let names = group_member_name_map(state, group).await;
    let model_configuration = group_model_configuration(state, group, config).await;
    let mut payload = session_group::group_to_json(group, &names);
    attach_group_model_configuration(&mut payload, &model_configuration, config, config_revision);
    payload
}

/// Build the full group JSON (for HTTP responses) with member names resolved once.
pub(crate) async fn group_json(state: &AppState, group: &SessionGroup) -> Value {
    let model_status_guard = crate::CONFIG_FILE_LOCK.read().await;
    let (config, config_revision) = state.config_snapshot_with_revision();
    let payload = group_json_with_config(state, group, &config, config_revision).await;
    drop(model_status_guard);
    payload
}

async fn group_info_json_with_config(
    state: &AppState,
    group: &SessionGroup,
    config: &Config,
    config_revision: u64,
) -> Value {
    let names = group_member_name_map(state, group).await;
    let model_configuration = group_model_configuration(state, group, config).await;
    let mut payload = session_group::group_info_payload(group, &names);
    attach_group_model_configuration(&mut payload, &model_configuration, config, config_revision);
    payload
}

pub(crate) async fn group_info_json(state: &AppState, group: &SessionGroup) -> Value {
    let model_status_guard = crate::CONFIG_FILE_LOCK.read().await;
    let (config, config_revision) = state.config_snapshot_with_revision();
    let payload = group_info_json_with_config(state, group, &config, config_revision).await;
    drop(model_status_guard);
    payload
}

async fn group_history_json_with_config(
    state: &AppState,
    group: &SessionGroup,
    config: &Config,
    config_revision: u64,
) -> Value {
    let names = group_member_name_map(state, group).await;
    let model_configuration = group_model_configuration(state, group, config).await;
    let mut payload = session_group::group_history_payload(group, &names);
    attach_group_model_configuration(&mut payload, &model_configuration, config, config_revision);
    payload
}

pub(crate) async fn group_history_json(state: &AppState, group: &SessionGroup) -> Value {
    let model_status_guard = crate::CONFIG_FILE_LOCK.read().await;
    let (config, config_revision) = state.config_snapshot_with_revision();
    let payload = group_history_json_with_config(state, group, &config, config_revision).await;
    drop(model_status_guard);
    payload
}

/// Broadcast a reliable `group` info payload with member names resolved once.
pub(crate) async fn send_group_info(state: &AppState, group: &SessionGroup) {
    let payload = group_info_json(state, group).await;
    crate::send_group_client_event(state, &group.id, GroupClientEvent::reliable(payload)).await;
}

/// Broadcast a reliable `group_history` payload with member names resolved once.
pub(crate) async fn send_group_history(state: &AppState, group_id: &str, group: &SessionGroup) {
    crate::send_group_client_event(
        state,
        group_id,
        GroupClientEvent::reliable(group_history_json(state, group).await),
    )
    .await;
}

pub(crate) async fn collect_group_model_configuration_payloads(
    state: &AppState,
    config: &Config,
    config_revision: u64,
) -> Vec<(String, Value)> {
    let group_ids = {
        let clients = state.group_clients.lock().await;
        clients.keys().cloned().collect::<Vec<_>>()
    };
    let mut payloads = Vec::with_capacity(group_ids.len());
    for group_id in group_ids {
        let group = match session_group::load_group_from_storage_result(&group_id) {
            Ok(Some(group)) => group,
            Ok(None) => continue,
            Err(error) => {
                eprintln!(
                    "ERROR: failed to collect model configuration for group {group_id}: {error}"
                );
                continue;
            }
        };
        let payload =
            group_model_configuration_payload_with_config(state, &group, config, config_revision)
                .await;
        payloads.push((group_id, payload));
    }
    payloads
}

pub(crate) async fn send_group_model_configuration_payloads(
    state: &AppState,
    payloads: Vec<(String, Value)>,
) {
    futures::future::join_all(payloads.into_iter().map(|(group_id, payload)| async move {
        crate::send_group_client_event(state, &group_id, GroupClientEvent::reliable(payload)).await;
    }))
    .await;
}

fn spawn_target_run(
    state: Arc<AppState>,
    run: StartedRun,
    prompt: String,
    run_mode: AgentRunMode,
    summary_budget: usize,
) {
    let panic_state = Arc::clone(&state);
    let panic_run = run.clone();
    tokio::spawn(async move {
        let result = std::panic::AssertUnwindSafe(run_target_run(
            state,
            run,
            prompt,
            run_mode,
            summary_budget,
        ))
        .catch_unwind()
        .await;
        if result.is_err() {
            eprintln!(
                "ERROR: delegated session run panicked for session '{}'",
                panic_run.session_id
            );
            mark_started_run_failed_after_panic(panic_state.as_ref(), &panic_run).await;
        }
    });
}

async fn mark_started_run_failed_after_panic(state: &AppState, run: &StartedRun) {
    runtime_loop::release_agent_run_for_stop_requested(
        state,
        &run.session_id,
        &run.control.stop_requested,
    )
    .await;
    if let Some(group_id) = run.group_id.as_deref() {
        if let Err(error) = update_run_status(
            state,
            group_id,
            &run.run_id,
            "failed",
            None,
            Some("Target session run panicked.".to_string()),
        )
        .await
        {
            eprintln!("ERROR: failed to mark panicked group run as failed: {error}");
        }
        clear_group_run_control(&run.run_id);
    } else {
        update_direct_run_status(&run.run_id, DirectRunStatus::Failed);
    }
}

async fn mark_group_run_failed(state: &AppState, group_id: &str, run_id: &str, error: String) {
    if let Err(save_error) =
        update_run_status(state, group_id, run_id, "failed", None, Some(error)).await
    {
        eprintln!("ERROR: failed to mark group run as failed: {save_error}");
    }
    clear_group_run_control(run_id);
}

fn delegated_run_should_stop(
    state: &AppState,
    run_cancel: &CancellationToken,
    stop_requested: &AtomicBool,
) -> bool {
    state.shutdown.is_cancelled()
        || run_cancel.is_cancelled()
        || stop_requested.load(Ordering::Relaxed)
}

async fn mark_started_run_stopped(state: &AppState, run: &StartedRun) {
    if let Some(group_id) = run.group_id.as_deref() {
        let _ = update_run_status(state, group_id, &run.run_id, "stopped", None, None)
            .await
            .map_err(|error| eprintln!("ERROR: failed to stop group run: {error}"));
        clear_group_run_control(&run.run_id);
    } else {
        update_direct_run_status(&run.run_id, DirectRunStatus::Stopped);
    }
}

async fn mark_started_run_failed(state: &AppState, run: &StartedRun, error: String) {
    if let Some(group_id) = run.group_id.as_deref() {
        mark_group_run_failed(state, group_id, &run.run_id, error).await;
    } else {
        update_direct_run_status(&run.run_id, DirectRunStatus::Failed);
    }
}

async fn release_reserved_run_if_stopped(
    state: &Arc<AppState>,
    run: &StartedRun,
    reservation: &runtime_loop::AgentRunReservation,
    run_cancel: &CancellationToken,
    stop_requested: &AtomicBool,
) -> bool {
    if !delegated_run_should_stop(state.as_ref(), run_cancel, stop_requested) {
        return false;
    }
    runtime_loop::release_agent_run_reservation(state, &run.session_id, reservation).await;
    mark_started_run_stopped(state.as_ref(), run).await;
    true
}

async fn run_target_run(
    state: Arc<AppState>,
    run: StartedRun,
    prompt: String,
    run_mode: AgentRunMode,
    summary_budget: usize,
) {
    let run_cancel = run.control.cancel.clone();
    let stop_requested = Arc::clone(&run.control.stop_requested);
    let lock = session_control_lock(&state, &run.session_id).await;
    let _guard = lock.lock().await;
    // `delete_session_with_safety_checks` holds this same session-control lock across its
    // destructive ops, so if a delete completed while this run waited on the lock the target
    // is gone. Abort instead of recreating it as an orphan via `ensure_session_ready`. This
    // closes the residual window left after the delete gate's under-lock re-check.
    let target_exists = if state.sessions.lock().await.contains_key(&run.session_id) {
        true
    } else {
        match session_store::load_session_from_storage_result(&run.session_id) {
            Ok(session) => session.is_some(),
            Err(error) => {
                eprintln!(
                    "ERROR: failed to verify delegated target session {}: {error}",
                    run.session_id
                );
                if let Some(group_id) = run.group_id.as_deref() {
                    mark_group_run_failed(
                        state.as_ref(),
                        group_id,
                        &run.run_id,
                        "Local storage is unavailable.".to_string(),
                    )
                    .await;
                } else {
                    update_direct_run_status(&run.run_id, DirectRunStatus::Failed);
                }
                return;
            }
        }
    };
    if !target_exists {
        if let Some(group_id) = run.group_id.as_deref() {
            mark_group_run_failed(
                state.as_ref(),
                group_id,
                &run.run_id,
                "Target session no longer exists.".to_string(),
            )
            .await;
        } else {
            update_direct_run_status(&run.run_id, DirectRunStatus::Failed);
        }
        return;
    }
    if let Some(group_id) = run.group_id.as_deref() {
        match group_run_is_active(group_id, &run.run_id).await {
            Ok(true) => {}
            Ok(false) => {
                clear_group_run_control(&run.run_id);
                return;
            }
            Err(error) => {
                eprintln!("ERROR: failed to read delegated group run state: {error}");
                clear_group_run_control(&run.run_id);
                return;
            }
        }
    }
    if run.group_id.is_none() && !direct_run_is_active(&run.run_id) {
        return;
    }
    wait_until_session_idle(&state, &run.session_id, &run_cancel).await;
    if delegated_run_should_stop(state.as_ref(), &run_cancel, &stop_requested) {
        mark_started_run_stopped(state.as_ref(), &run).await;
        return;
    }
    if let Some(group_id) = run.group_id.as_deref() {
        match group_run_is_active(group_id, &run.run_id).await {
            Ok(true) => {}
            Ok(false) => {
                clear_group_run_control(&run.run_id);
                return;
            }
            Err(error) => {
                eprintln!("ERROR: failed to re-read delegated group run state: {error}");
                clear_group_run_control(&run.run_id);
                return;
            }
        }
    }
    if run.group_id.is_none() && !direct_run_is_active(&run.run_id) {
        return;
    }
    let connection_id = state.next_connection_id.fetch_add(1, Ordering::Relaxed);
    // Acquire the agent-run reservation BEFORE persisting/broadcasting the
    // queued->running transition. A group's "running" transition both writes to
    // disk and emits a reliable group_run_status event to every group socket; if
    // we advertised "running" first and then lost the reservation race we would
    // broadcast a phantom running->failed for a run that executed zero cycles.
    let reservation = match runtime_loop::try_reserve_agent_run(
        &state,
        &run.session_id,
        connection_id,
        &run_cancel,
        &stop_requested,
    )
    .await
    {
        Some(reservation) => reservation,
        None => {
            if let Some(group_id) = run.group_id.as_deref() {
                // The run is still "queued" here, so this is a direct
                // queued->failed transition; "running" is never advertised.
                mark_group_run_failed(
                    state.as_ref(),
                    group_id,
                    &run.run_id,
                    "Target session already has an active run.".to_string(),
                )
                .await;
            } else {
                update_direct_run_status(&run.run_id, DirectRunStatus::Failed);
            }
            return;
        }
    };

    let model_snapshot = match crate::session_model_snapshot(&state, &run.session_id).await {
        Some(snapshot) if snapshot.explicit => snapshot,
        _ => {
            runtime_loop::release_agent_run_reservation(&state, &run.session_id, &reservation)
                .await;
            mark_started_run_failed(
                state.as_ref(),
                &run,
                "Explicit model configuration is required for the target session.".to_string(),
            )
            .await;
            return;
        }
    };

    if let Some(group_id) = run.group_id.as_deref() {
        match update_run_status(&state, group_id, &run.run_id, "running", None, None).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                runtime_loop::release_agent_run_reservation(&state, &run.session_id, &reservation)
                    .await;
                clear_group_run_control(&run.run_id);
                return;
            }
            Err(error) => {
                eprintln!("ERROR: failed to mark group run as running: {error}");
                runtime_loop::release_agent_run_reservation(&state, &run.session_id, &reservation)
                    .await;
                clear_group_run_control(&run.run_id);
                return;
            }
        }
    } else if !update_direct_run_status(&run.run_id, DirectRunStatus::Running) {
        runtime_loop::release_agent_run_reservation(&state, &run.session_id, &reservation).await;
        return;
    }

    if release_reserved_run_if_stopped(&state, &run, &reservation, &run_cancel, &stop_requested)
        .await
    {
        return;
    }

    // Capture the delegated user message's content as a stable boundary before it
    // is moved into the session. We use its identity (not an absolute index) to
    // locate this run's reply afterwards, so the lookup survives mid-run context
    // compression that shrinks/re-indexes the message list.
    let boundary_user_message = prompt.clone();
    match append_target_session_message(&state, &run.session_id, prompt).await {
        Ok(_index) => {}
        Err(error) => {
            runtime_loop::release_agent_run_reservation(&state, &run.session_id, &reservation)
                .await;
            if let Some(group_id) = run.group_id.as_deref() {
                mark_group_run_failed(state.as_ref(), group_id, &run.run_id, error).await;
            } else {
                update_direct_run_status(&run.run_id, DirectRunStatus::Failed);
            }
            return;
        }
    };

    let (live_tx, mut live_rx) = mpsc::channel::<Value>(crate::LIVE_EVENT_CHANNEL_CAPACITY);
    let dispatch_state = Arc::clone(&state);
    let dispatch_session_id = run.session_id.clone();
    let dispatch_log_session_id = dispatch_session_id.clone();
    let dispatch_group_id = run.group_id.clone();
    let dispatch_run_id = run.run_id.clone();
    let dispatcher = tokio::spawn(async move {
        let result = std::panic::AssertUnwindSafe(async move {
            while let Some(event) = live_rx.recv().await {
                crate::dispatch_live_event(
                    &dispatch_state,
                    &dispatch_session_id,
                    connection_id,
                    event.clone(),
                )
                .await;
                if let Some(group_id) = dispatch_group_id.as_deref() {
                    forward_group_live_event(
                        &dispatch_state,
                        group_id,
                        &dispatch_run_id,
                        &dispatch_session_id,
                        event,
                    )
                    .await;
                }
            }
        })
        .catch_unwind()
        .await;
        if result.is_err() {
            eprintln!(
                "ERROR: delegated live-event dispatcher panicked for session '{}'",
                dispatch_log_session_id
            );
        }
    });
    let (_inbound_tx, mut inbound_rx) = mpsc::channel::<String>(1);
    let outcome = runtime_loop::run_agent_session(
        &state,
        &run.session_id,
        connection_id,
        &run_cancel,
        &live_tx,
        &mut inbound_rx,
        &stop_requested,
        run_mode,
        Some(reservation),
        Some(model_snapshot),
    )
    .await;
    drop(live_tx);
    if let Err(error) = dispatcher.await {
        eprintln!("ERROR: delegated live-event dispatcher task failed: {error}");
    }

    let assistant_excerpt = latest_assistant_excerpt_after(
        &state,
        &run.session_id,
        &boundary_user_message,
        summary_budget,
    )
    .await;
    let result_excerpt = assistant_excerpt.clone().unwrap_or_else(|| {
        if outcome.shutting_down {
            "Server shutdown before session produced a final response.".to_string()
        } else {
            "Session run finished without assistant text.".to_string()
        }
    });
    if let Some(group_id) = run.group_id.as_deref() {
        let (status, error) = if outcome.shutting_down || outcome.run_stopped {
            ("stopped", None)
        } else if outcome.run_failed {
            ("failed", Some("Target session run failed.".to_string()))
        } else {
            ("completed", None)
        };
        match update_run_status(
            &state,
            group_id,
            &run.run_id,
            status,
            Some(result_excerpt.clone()),
            error,
        )
        .await
        {
            Ok(Some(_)) => {
                if group_run_status_records_result(status) {
                    let recorded = record_group_session_result(
                        &state,
                        group_id,
                        &run.run_id,
                        &run.session_id,
                        result_excerpt.clone(),
                    )
                    .await;
                    if recorded {
                        dispatch_group_mentions_from_session_result(
                            &state,
                            group_id,
                            &run.session_id,
                            &result_excerpt,
                            run_mode,
                            summary_budget,
                            run.mention_depth,
                        )
                        .await;
                    }
                } else if status == "failed"
                    && let Some(output) = assistant_excerpt.as_deref()
                {
                    // A failed run may still have produced a useful partial answer.
                    // Persist that genuine output as a group message (the generic
                    // failure error is surfaced separately) but do not trigger
                    // @mention follow-ups for a failed run.
                    record_group_session_result(
                        &state,
                        group_id,
                        &run.run_id,
                        &run.session_id,
                        output.to_string(),
                    )
                    .await;
                }
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!("ERROR: failed to persist group run completion: {error}");
            }
        }
        clear_group_run_control(&run.run_id);
    } else {
        let status = if outcome.shutting_down || outcome.run_stopped {
            DirectRunStatus::Stopped
        } else if outcome.run_failed {
            DirectRunStatus::Failed
        } else {
            DirectRunStatus::Completed
        };
        update_direct_run_status(&run.run_id, status);
    }
}

async fn dispatch_to_sessions(
    state: &Arc<AppState>,
    request: DispatchRequest,
) -> Result<Vec<StartedRun>, String> {
    let canonical_targets = request.targets;
    if canonical_targets.is_empty() {
        return Err("No target sessions were selected.".to_string());
    }

    let stored_prompt = stored_group_run_prompt(&request.message);
    let mut runs = Vec::new();
    if let Some(group_id) = request.group_id.as_deref() {
        let (group_message, group_runs) = mutate_group_result(group_id, |group| {
            validate_group_targets(group_id, &group.members, &canonical_targets)?;
            let now = now_epoch();
            let group_message = request.group_message.as_ref().map(|message| {
                append_group_message(
                    group,
                    &message.role,
                    message.session_id.clone(),
                    request.message.clone(),
                    message.turn_id.clone(),
                    None,
                )
            });
            let mut out = Vec::new();
            for target in &canonical_targets {
                let run = GroupRun {
                    id: next_id("grun"),
                    group_id: group_id.to_string(),
                    session_id: target.clone(),
                    status: "queued".to_string(),
                    prompt: stored_prompt.clone(),
                    result_excerpt: None,
                    error: None,
                    created_at: now,
                    updated_at: now,
                    completed_at: None,
                };
                group.runs.push(run.clone());
                out.push(run);
            }
            Ok::<(Option<GroupMessage>, Vec<GroupRun>), String>((group_message, out))
        })
        .await?
        .1;
        if let Some(message) = group_message.as_ref() {
            crate::send_group_client_event(
                state,
                group_id,
                GroupClientEvent::reliable(json!({
                    "type": "group_message",
                    "group_id": group_id,
                    "message": message,
                })),
            )
            .await;
            let group = session_group::load_group_from_storage_result(group_id)?
                .ok_or_else(|| format!("Group '{}' not found", group_id))?;
            send_group_history(state, group_id, &group).await;
        }
        for group_run in group_runs {
            let control = DelegatedRunControl {
                cancel: state.shutdown.child_token(),
                stop_requested: Arc::new(AtomicBool::new(false)),
            };
            register_group_run_control(&group_run.id, group_id, &group_run.session_id, &control);
            crate::send_group_client_event(
                state,
                group_id,
                GroupClientEvent::reliable(json!({
                    "type": "group_run_started",
                    "group_id": group_id,
                    "run": group_run,
                })),
            )
            .await;
            runs.push(StartedRun {
                run_id: group_run.id,
                group_id: Some(group_id.to_string()),
                optional_reply: target_set_contains_session(
                    &request.optional_targets,
                    &group_run.session_id,
                ),
                mention_depth: request.mention_depth,
                session_id: group_run.session_id,
                control,
            });
        }
    } else {
        runs.extend(canonical_targets.into_iter().map(|session_id| {
            let run_id = next_id("run");
            let control = DelegatedRunControl {
                cancel: state.shutdown.child_token(),
                stop_requested: Arc::new(AtomicBool::new(false)),
            };
            register_direct_run(&run_id, &session_id, &control);
            StartedRun {
                run_id,
                group_id: None,
                optional_reply: false,
                mention_depth: 0,
                session_id,
                control,
            }
        }));
    }

    let group_context = request
        .group_id
        .as_deref()
        .map(|group_id| target_group_context(group_id, request.summary_budget))
        .transpose()?;

    for run in runs.clone() {
        let prompt = target_prompt(
            request.group_id.as_deref(),
            &request.message,
            group_context.as_deref(),
            run.optional_reply,
        );
        spawn_target_run(
            Arc::clone(state),
            run,
            prompt,
            request.run_mode,
            request.summary_budget,
        );
    }

    if request.wait {
        wait_for_runs(state, &runs, request.summary_budget).await?;
    }
    Ok(runs)
}

async fn wait_for_runs(
    state: &AppState,
    runs: &[StartedRun],
    _summary_budget: usize,
) -> Result<(), String> {
    let timeout = state.config().sub_agent_timeout;
    let deadline = run_wait_deadline(timeout);
    loop {
        let group_snapshots = runs
            .iter()
            .filter_map(|run| run.group_id.as_ref())
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|group_id| {
                session_group::load_group_from_storage_result(&group_id)
                    .map(|group| (group_id, group))
            })
            .collect::<Result<HashMap<_, _>, _>>();
        let group_snapshots = group_snapshots?;
        let mut complete = true;
        for run in runs {
            let active = if let Some(group_id) = run.group_id.as_deref() {
                let status = group_snapshots
                    .get(group_id)
                    .and_then(|group| group.as_ref())
                    .and_then(|group| group.runs.iter().find(|item| item.id == run.run_id))
                    .map(|run| run.status.clone())
                    .unwrap_or_else(|| "failed".to_string());
                matches!(status.as_str(), "queued" | "running")
            } else {
                !direct_run_status(&run.run_id).is_terminal()
            };
            if active {
                complete = false;
                break;
            }
        }
        if complete || deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Ok(())
}

fn run_wait_deadline(timeout: Duration) -> Option<tokio::time::Instant> {
    if timeout.is_zero() {
        None
    } else {
        Some(tokio::time::Instant::now() + timeout)
    }
}

pub(crate) async fn handle_group_socket_message(
    state: &Arc<AppState>,
    group_id: &str,
    payload: GroupSocketDispatch,
) -> Result<(), String> {
    let text = payload.text.trim().to_string();
    if text.is_empty() {
        return Err("Group message cannot be empty.".to_string());
    }
    let run_mode = parse_run_mode(&payload.run_mode)?;
    if run_mode == AgentRunMode::PlanOnly {
        return Err("group_plan_mode_unsupported".to_string());
    }
    validate_message_len(&text)?;
    let group = session_group::load_group_from_storage_result(group_id)?
        .ok_or_else(|| format!("Group '{}' not found", group_id))?;
    let mut optional_targets = HashSet::new();
    let dispatch_targets = if payload.start_runs {
        if payload.target_mode == "selected"
            && payload.targets.len() > SESSION_CONTROL_TARGETS_MAX_ITEMS
        {
            return Err(format!(
                "session_control error: targets exceeds {} item(s)",
                SESSION_CONTROL_TARGETS_MAX_ITEMS
            ));
        }
        let direct_mentions = mentions_from_text(&text, &group.members);
        let mentions_all = text_mentions_all(&text);
        let targets = match payload.target_mode.as_str() {
            "selected" => normalize_target_ids(payload.targets),
            "mentions" if mentions_all => group.members.clone(),
            "mentions" => direct_mentions.clone(),
            "all" | "" => group.members.clone(),
            other => {
                return Err(format!(
                    "Invalid target_mode '{}'. Use selected, all, or mentions.",
                    other
                ));
            }
        };
        if payload.target_mode == "mentions" && targets.is_empty() {
            return Err("No valid @session-id mentions were found.".to_string());
        }
        // Optional replies (a member may answer NO_REPLY without persisting a group
        // message) apply only to the `@all` overflow: members covered by an `@all`
        // mention but not directly `@session-id`'d. A plain "all"/"selected" broadcast
        // — or a stray "@all" in a non-mentions message — must still require every
        // dispatched target to reply.
        if payload.target_mode == "mentions" && mentions_all {
            let forced = direct_mentions.into_iter().collect::<HashSet<_>>();
            optional_targets = targets
                .iter()
                .filter(|target| !forced.contains(*target))
                .cloned()
                .collect();
        }
        let max_targets = if payload.target_mode == "selected" {
            SESSION_CONTROL_TARGETS_MAX_ITEMS
        } else {
            SESSION_CONTROL_MEMBERS_MAX_ITEMS
        };
        let targets = prepare_dispatch_targets(state, Some(group_id), targets, max_targets).await?;
        ensure_explicit_target_models(state, &targets).await?;
        Some(targets)
    } else {
        match payload.target_mode.as_str() {
            "selected" | "mentions" | "all" | "" => {}
            other => {
                return Err(format!(
                    "Invalid target_mode '{}'. Use selected, all, or mentions.",
                    other
                ));
            }
        }
        None
    };
    let turn_id = next_id("turn");
    if let Some(targets) = dispatch_targets {
        dispatch_to_sessions(
            state,
            DispatchRequest {
                group_id: Some(group_id.to_string()),
                targets,
                optional_targets,
                message: text,
                group_message: Some(DispatchGroupMessage {
                    role: "user".to_string(),
                    session_id: None,
                    turn_id: Some(turn_id),
                }),
                run_mode,
                wait: false,
                summary_budget: 4_000,
                mention_depth: 0,
            },
        )
        .await?;
    } else {
        let message = mutate_group(group_id, |group| {
            append_group_message(
                group,
                "user",
                None,
                text.clone(),
                Some(turn_id.clone()),
                None,
            )
        })
        .await?
        .1;
        crate::send_group_client_event(
            state,
            group_id,
            GroupClientEvent::reliable(json!({
                "type": "group_message",
                "group_id": group_id,
                "message": message,
            })),
        )
        .await;
        send_group_history(
            state,
            group_id,
            &session_group::load_group_from_storage_result(group_id)?
                .ok_or_else(|| format!("Group '{}' not found", group_id))?,
        )
        .await;
    }
    Ok(())
}

pub(crate) async fn handle_group_socket_stop(
    state: &AppState,
    group_id: &str,
    targets: Vec<String>,
) -> Result<String, String> {
    if targets.len() > SESSION_CONTROL_TARGETS_MAX_ITEMS {
        return Err(format!(
            "session_control error: targets exceeds {} item(s)",
            SESSION_CONTROL_TARGETS_MAX_ITEMS
        ));
    }
    stop_group_runs(state, group_id, targets).await
}

fn collect_group_summary(group: &SessionGroup) -> String {
    let mut lines = vec![format!(
        "Group {} ({}) members: {}",
        group.id,
        group.name,
        if group.members.is_empty() {
            "none".to_string()
        } else {
            group.members.join(", ")
        }
    )];
    if group.runs.is_empty() {
        lines.push("Runs: none".to_string());
    } else {
        lines.push("Runs:".to_string());
        for run in group.runs.iter().rev().take(20).rev() {
            let result = run
                .result_excerpt
                .as_deref()
                .map(redact_profile_summary_text)
                .unwrap_or_default();
            let error = run
                .error
                .as_deref()
                .map(redact_profile_summary_text)
                .unwrap_or_default();
            lines.push(format!(
                "- {} session={} status={} result={} error={}",
                run.id, run.session_id, run.status, result, error
            ));
        }
    }
    if !group.messages.is_empty() {
        lines.push("Recent messages:".to_string());
        for message in group.messages.iter().rev().take(10).rev() {
            let who = message.session_id.as_deref().unwrap_or(&message.role);
            lines.push(format!(
                "- {}: {}",
                who,
                redact_profile_summary_text(&message.content)
            ));
        }
    }
    lines.join("\n")
}

async fn session_list_output(state: &AppState) -> Result<String, String> {
    struct LoadedSessionListSnapshot {
        summary: SessionSummary,
        model: String,
    }

    let config = state.config();
    let mut summaries =
        session_store::list_saved_session_summaries_result(&session_store::sessions_dir())
            .map_err(|_| storage_protected_control_error())?;
    let loaded_sessions = {
        let sessions = state.sessions.lock().await;
        sessions
            .values()
            .map(|session| {
                (
                    session.id.clone(),
                    LoadedSessionListSnapshot {
                        summary: SessionSummary::from_session(session),
                        model: session.effective_model(&config.model).to_string(),
                    },
                )
            })
            .collect::<HashMap<_, _>>()
    };
    for session in loaded_sessions.values() {
        if !summaries
            .iter()
            .any(|summary| summary.id == session.summary.id)
        {
            summaries.push(session.summary.clone());
        }
    }
    session_store::sort_session_summaries(&mut summaries);
    let group_statuses = active_group_run_statuses_by_session();
    let direct_statuses = direct_run_statuses_by_session();
    let active_session_ids = {
        let active_runs = state.active_runs.lock().await;
        active_runs.keys().cloned().collect::<HashSet<_>>()
    };
    let mut lines = vec![
        "Sessions:".to_string(),
        format!(
            "TaskPlan: {} (global setting)",
            if config.enable_task_plan {
                "enabled"
            } else {
                "disabled"
            }
        ),
    ];
    for summary in summaries {
        let session = loaded_sessions.get(&summary.id);
        let model = if let Some(session) = session {
            session.model.clone()
        } else {
            summary
                .model_override
                .clone()
                .unwrap_or_else(|| config.model.clone())
        };
        let status = session_runtime_status_from_snapshots(
            &active_session_ids,
            &summary.id,
            direct_statuses.get(&summary.id).copied(),
            group_statuses.get(&summary.id).map(String::as_str),
        );
        lines.push(format!(
            "- {} ({}) model={} status={} skills={} mcp_tools={} updated_at={}{}",
            summary.id,
            summary.name,
            model,
            status,
            "unknown",
            "unknown",
            summary.updated_at,
            if summary.corrupt { " corrupt" } else { "" }
        ));
        lines.push("  agent: unknown (use describe_session)".to_string());
        lines.push("  user: unknown (use describe_session)".to_string());
    }
    Ok(lines.join("\n"))
}

fn group_list_output() -> Result<String, String> {
    let mut lines = vec!["Session groups:".to_string()];
    for summary in session_group::list_saved_group_summaries_result()
        .map_err(|_| storage_protected_control_error())?
    {
        lines.push(format!(
            "- {} ({}) members={} messages={} running={}{}",
            summary.id,
            summary.name,
            summary.members,
            summary.messages,
            summary.running,
            if summary.corrupt { " corrupt" } else { "" }
        ));
    }
    Ok(lines.join("\n"))
}

async fn load_session_for_description(state: &AppState, target: &str) -> Result<Session, String> {
    let target = session_store::validate_session_id(target)?.to_string();
    {
        let sessions = state.sessions.lock().await;
        if let Some(session_id) = crate::find_loaded_session_id(&sessions, &target)
            && let Some(session) = sessions.get(&session_id)
        {
            return Ok(session.clone());
        }
    }
    session_store::load_session_from_storage_result(&target)
        .map_err(|_| storage_protected_control_error())?
        .ok_or_else(|| format!("Session '{}' not found", target))
}

fn tool_args_sections(value: &Value) -> Vec<String> {
    let mut seen = HashSet::new();
    let sections = value
        .get("sections")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|section| !section.is_empty())
        .map(str::to_string)
        .filter(|section| seen.insert(section.clone()))
        .collect::<Vec<_>>();
    if sections.is_empty() {
        vec![
            "profile".to_string(),
            "capabilities".to_string(),
            "runtime".to_string(),
        ]
    } else {
        sections
    }
}

fn tool_args_max_chars(value: &Value, default_value: usize) -> usize {
    value
        .get("max_chars")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(default_value)
        .clamp(1_000, 20_000)
}

fn render_profile_section(session: &Session) -> Vec<String> {
    let profile = summarize_session_profile(&session.workspace);
    vec![
        "Profile:".to_string(),
        format_profile_line("agent_summary", &profile.agent),
        format_profile_line("identity_summary", &profile.identity),
        format_profile_line("user_summary", &profile.user),
        format_profile_line("style_summary", &profile.style),
    ]
}

fn source_label(source: SkillSource) -> &'static str {
    match source {
        SkillSource::System => "system",
        SkillSource::Global => "global",
        SkillSource::Session => "session",
    }
}

fn builtin_tool_names_for_session(config: &Config, session: &Session) -> Vec<&'static str> {
    let model = session.effective_model(&config.model);
    let mut tools = tools::available_builtin_tool_specs(config, Some(model))
        .into_iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>();
    if !crate::subagents::discovery::discover_all_agents(&session.workspace).is_empty() {
        tools.push(tools::TOOL_NAME_TASK);
        tools.push(tools::TOOL_NAME_ORCHESTRATE);
    }
    if session.id == MAIN_SESSION_ID {
        tools.push(tools::TOOL_NAME_SESSION_CONTROL);
    }
    tools
}

fn render_capabilities_section(state: &AppState, session: &Session) -> Vec<String> {
    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    let skills = enabled_skills_for_session(session);
    let mcp_tools = enabled_mcp_tools_for_session(&config, session);
    let mut lines = vec![
        "Capabilities:".to_string(),
        format!("- model: {model}"),
        format!("- image_input: {}", config.model_supports_image(&model)),
        format!(
            "- builtin_tools: {}",
            builtin_tool_names_for_session(&config, session).join(", ")
        ),
    ];
    if skills.is_empty() {
        lines.push("- skills: none".to_string());
    } else {
        let mut rendered = skills
            .iter()
            .map(|skill| format!("{} [{}]", skill.name, source_label(skill.source)))
            .collect::<Vec<_>>();
        rendered.sort();
        lines.push(format!("- skills: {}", rendered.join(", ")));
    }
    if mcp_tools.is_empty() {
        lines.push("- mcp_tools: none".to_string());
    } else {
        let mut rendered = mcp_tools
            .iter()
            .map(|tool| {
                format!(
                    "{} server={} raw={} read_only={}",
                    tool.exposed_name,
                    tool.server_name,
                    tool.raw_name,
                    tools::mcp::is_read_only_tool_descriptor(tool)
                )
            })
            .collect::<Vec<_>>();
        rendered.sort();
        lines.push(format!("- mcp_tools: {}", rendered.join(", ")));
    }
    lines
}

async fn render_runtime_section(
    state: &AppState,
    session: &Session,
    groups: &[SessionGroup],
) -> Vec<String> {
    let group_status = group_run_status_from_groups(&session.id, groups);
    let status = session_runtime_status(state, &session.id, group_status.as_deref()).await;
    let direct_runs = direct_runs_for_session(&session.id);
    let mut lines = vec![
        "Runtime:".to_string(),
        format!(
            "- active: {}",
            matches!(status.as_str(), "queued" | "running")
        ),
        format!("- status: {status}"),
    ];
    if session.failed_tool_results.is_empty() {
        lines.push("- recent_tool_failures: none".to_string());
    } else {
        let mut failures = session
            .failed_tool_results
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        failures.sort();
        failures.truncate(10);
        lines.push(format!("- recent_tool_failures: {}", failures.join(", ")));
    }
    if direct_runs.is_empty() {
        lines.push("- direct_runs: none".to_string());
    } else {
        let direct = direct_runs
            .iter()
            .take(5)
            .map(|(run_id, status, updated_at)| format!("{run_id}:{status:?}@{updated_at}"))
            .collect::<Vec<_>>();
        lines.push(format!("- direct_runs: {}", direct.join(", ")));
    }

    let mut group_runs = Vec::new();
    for group in groups {
        for run in group.runs.iter().filter(|run| run.session_id == session.id) {
            group_runs.push((
                group.id.clone(),
                group.name.clone(),
                run.id.clone(),
                run.status.clone(),
                run.updated_at,
                run.result_excerpt.clone(),
                run.error.clone(),
            ));
        }
    }
    group_runs.sort_by(|a, b| b.4.cmp(&a.4).then_with(|| a.0.cmp(&b.0)));
    if let Some((group_id, group_name, run_id, status, updated_at, result, error)) =
        group_runs.first()
    {
        let result = result
            .as_deref()
            .map(redact_profile_summary_text)
            .unwrap_or_default();
        let error = error
            .as_deref()
            .map(redact_profile_summary_text)
            .unwrap_or_default();
        lines.push(format!(
            "- last_group_run: group={} ({}) run={} status={} updated_at={} result={} error={}",
            group_id, group_name, run_id, status, updated_at, result, error
        ));
    } else {
        lines.push("- last_group_run: none".to_string());
    }
    lines
}

fn render_groups_section(session: &Session, groups: &[SessionGroup]) -> Vec<String> {
    let mut lines = vec!["Groups:".to_string()];
    if groups.is_empty() {
        lines.push("- none".to_string());
        return lines;
    }
    for group in groups.iter().take(10) {
        let active = group
            .runs
            .iter()
            .filter(|run| {
                run.session_id == session.id && matches!(run.status.as_str(), "queued" | "running")
            })
            .count();
        lines.push(format!(
            "- {} ({}) member={} active_runs={} updated_at={}",
            group.id,
            group.name,
            group.members.iter().any(|member| member == &session.id),
            active,
            group.updated_at
        ));
    }
    lines
}

async fn describe_session_from_tool(state: &AppState, args: &Value) -> Result<String, String> {
    validate_tool_array_len(args, "sections", SESSION_CONTROL_SECTIONS_MAX_ITEMS)?;
    let target = tool_args_string(args, "target")
        .or_else(|| tool_args_string(args, "session_id"))
        .ok_or_else(|| "session_control error: target is required".to_string())?;
    let session = load_session_for_description(state, &target).await?;
    let sections = tool_args_sections(args);
    let allowed = ["profile", "capabilities", "runtime", "groups"]
        .into_iter()
        .collect::<HashSet<_>>();
    let mut invalid = sections
        .iter()
        .filter(|section| !allowed.contains(section.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    invalid.sort();
    invalid.dedup();
    if !invalid.is_empty() {
        return Err(format!(
            "session_control error: invalid section(s): {}",
            invalid.join(", ")
        ));
    }

    let mut lines = vec![format!("Session {} ({})", session.id, session.name)];
    let groups = if sections
        .iter()
        .any(|section| matches!(section.as_str(), "runtime" | "groups"))
    {
        all_groups_for_session(&session.id)?
    } else {
        Vec::new()
    };
    for section in sections {
        if lines.len() > 1 {
            lines.push(String::new());
        }
        match section.as_str() {
            "profile" => lines.extend(render_profile_section(&session)),
            "capabilities" => lines.extend(render_capabilities_section(state, &session)),
            "runtime" => lines.extend(render_runtime_section(state, &session, &groups).await),
            "groups" => lines.extend(render_groups_section(&session, &groups)),
            _ => {}
        }
    }
    let mut output = lines.join("\n");
    crate::truncate_safe(&mut output, tool_args_max_chars(args, 6_000));
    Ok(output)
}

fn sanitize_profile_input(field: &str, value: &str, max_chars: usize) -> Result<String, String> {
    if value.chars().count() > max_chars {
        return Err(format!(
            "session_control error: {field} exceeds {max_chars} characters"
        ));
    }
    let mut out = value
        .chars()
        .filter(|ch| *ch == '\n' || *ch == '\t' || !ch.is_control())
        .collect::<String>();
    out = out.trim().to_string();
    if out.chars().count() > max_chars {
        return Err(format!(
            "session_control error: {field} exceeds {max_chars} characters"
        ));
    }
    Ok(redact_profile_persisted_text(&out))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SecretValueContinuation {
    IndentedOrSingle { base_indent: usize },
    Block { base_indent: usize },
}

fn line_indent_len(line: &str) -> usize {
    line.len().saturating_sub(line.trim_start().len())
}

fn redact_profile_persisted_text(value: &str) -> String {
    let mut continuation: Option<SecretValueContinuation> = None;
    let mut lines = Vec::new();
    for line in value.lines() {
        if let Some(current) = continuation {
            if line.trim().is_empty() {
                lines.push(line.to_string());
                continue;
            }
            match current {
                SecretValueContinuation::IndentedOrSingle { base_indent } => {
                    lines.push(redact_profile_line_as_value(line));
                    if line_indent_len(line) > base_indent {
                        continuation = Some(SecretValueContinuation::Block { base_indent });
                    } else {
                        continuation = None;
                    }
                    continue;
                }
                SecretValueContinuation::Block { base_indent } => {
                    if line_indent_len(line) > base_indent {
                        lines.push(redact_profile_line_as_value(line));
                        continue;
                    }
                }
            }
        }
        let secret_continuation = profile_secret_value_continuation(line);
        lines.push(redact_profile_summary_text(line));
        continuation = secret_continuation;
    }
    lines.join("\n")
}

fn profile_secret_value_continuation(line: &str) -> Option<SecretValueContinuation> {
    let trimmed = line.trim().trim_start_matches(['-', '*']).trim();
    let (key, value) = trimmed.split_once(':')?;
    let value = value.trim();
    if !is_profile_secret_key(key.trim()) {
        return None;
    }
    if value.is_empty() {
        Some(SecretValueContinuation::IndentedOrSingle {
            base_indent: line_indent_len(line),
        })
    } else if matches!(value, "|" | ">" | "|-" | ">-" | "|+" | ">+") {
        Some(SecretValueContinuation::Block {
            base_indent: line_indent_len(line),
        })
    } else {
        None
    }
}

fn redact_profile_line_as_value(line: &str) -> String {
    let indent_len = line.len().saturating_sub(line.trim_start().len());
    let indent = &line[..indent_len];
    let trimmed = line.trim_start();
    if trimmed.starts_with("- ") {
        format!("{indent}- [redacted]")
    } else {
        format!("{indent}[redacted]")
    }
}

fn yaml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn write_text_file(path: &Path, content: &str) -> Result<(), String> {
    std::fs::write(path, content).map_err(|error| {
        format!(
            "Failed to write session profile file {}: {error}",
            path.display()
        )
    })
}

fn profile_doc(summary: &str, heading: &str, body: &str) -> String {
    format!(
        "---\nsummary: {}\n---\n\n# {heading}\n\n{body}\n",
        yaml_quote(&limit_summary_text(summary, 180))
    )
}

fn append_controlled_agent_profile(
    workspace: &Path,
    purpose: Option<&str>,
    agent_notes: Option<&str>,
) -> Result<(), String> {
    if purpose.is_none() && agent_notes.is_none() {
        return Ok(());
    }
    let path = workspace.join("AGENTS.md");
    let mut content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(format!(
                "Failed to read session profile file {}: {error}",
                path.display()
            ));
        }
    };
    if let Some(purpose) = purpose {
        content = replace_or_insert_frontmatter_summary(&content, purpose);
    }
    content.push_str("\n\n## Session Control Profile\n\n");
    if let Some(purpose) = purpose {
        content.push_str("### Purpose\n\n");
        content.push_str(purpose);
        content.push('\n');
    }
    if let Some(agent_notes) = agent_notes {
        content.push_str("\n### Agent Notes\n\n");
        content.push_str(agent_notes);
        content.push('\n');
    }
    write_text_file(&path, &content)
}

fn replace_or_insert_frontmatter_summary(content: &str, summary: &str) -> String {
    let summary_line = format!("summary: {}", yaml_quote(&limit_summary_text(summary, 180)));
    let had_bom = content.starts_with('\u{feff}');
    let content_without_bom = content.strip_prefix('\u{feff}').unwrap_or(content);
    let normalized = content_without_bom.replace("\r\n", "\n");
    if !normalized.starts_with("---\n") {
        let prefix = if had_bom { "\u{feff}" } else { "" };
        return format!("{prefix}---\n{summary_line}\n---\n\n{content_without_bom}");
    }
    let Some(end) = normalized[4..].find("\n---") else {
        let prefix = if had_bom { "\u{feff}" } else { "" };
        return format!("{prefix}---\n{summary_line}\n---\n\n{content_without_bom}");
    };
    let frontmatter_end = 4 + end;
    let frontmatter = &normalized[4..frontmatter_end];
    let rest = &normalized[frontmatter_end..];
    let mut replaced = false;
    let mut lines = Vec::new();
    for line in frontmatter.lines() {
        if line.trim_start().starts_with("summary:") {
            lines.push(summary_line.clone());
            replaced = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !replaced {
        lines.insert(0, summary_line);
    }
    let prefix = if had_bom { "\u{feff}" } else { "" };
    format!("{prefix}---\n{}{}", lines.join("\n"), rest)
}

fn initialize_created_session_profile(
    workspace: &Path,
    name: &str,
    purpose: Option<&str>,
    identity_profile: Option<&str>,
    user_profile: Option<&str>,
    style_profile: Option<&str>,
    agent_notes: Option<&str>,
) -> Result<(), String> {
    if let Some(identity_profile) = identity_profile.or(purpose) {
        let style = style_profile.unwrap_or("");
        let body =
            format!("- **Name:** {name}\n- **Role:** {identity_profile}\n- **Style:** {style}\n");
        write_text_file(
            &workspace.join("IDENTITY.md"),
            &profile_doc(identity_profile, "IDENTITY.md - Agent Profile", &body),
        )?;
    }
    if let Some(user_profile) = user_profile {
        let body = format!(
            "Keep durable user preferences here. Do not store secrets unless explicitly asked.\n\n## Preferences\n\n- {user_profile}\n"
        );
        write_text_file(
            &workspace.join("USER.md"),
            &profile_doc(user_profile, "USER.md - User Profile", &body),
        )?;
    }
    if let Some(style_profile) = style_profile {
        let body = format!("## Defaults\n\n- {style_profile}\n");
        write_text_file(
            &workspace.join("SOUL.md"),
            &profile_doc(style_profile, "SOUL.md - Working Style", &body),
        )?;
    }
    append_controlled_agent_profile(workspace, purpose, agent_notes)
}

async fn generate_available_session_id_for_tool(state: &AppState) -> Result<String, String> {
    for _ in 0..128 {
        let id = crate::generate_random_session_id()?;
        if session_store::validate_session_id(&id).is_err() {
            continue;
        }
        {
            let sessions = state.sessions.lock().await;
            if crate::find_loaded_session_id(&sessions, &id).is_some() {
                continue;
            }
        }
        if session_store::canonical_saved_session_id_result(&id)?.is_some() {
            continue;
        }
        if crate::session_workspace_path(&id).exists() {
            continue;
        }
        return Ok(id);
    }
    Err("session_control error: failed to generate a unique session id".to_string())
}

pub(crate) fn cleanup_failed_created_session(session_id: &str, workspace: &Path) {
    #[cfg(test)]
    {
        let _ =
            std::fs::remove_file(session_store::sessions_dir().join(format!("{session_id}.json")));
        let _ = std::fs::remove_file(
            session_store::sessions_dir().join(format!("{session_id}.json.tmp")),
        );
    }
    let expected_workspace = crate::session_workspace_path(session_id);
    if workspace != expected_workspace {
        return;
    }
    let Ok(metadata) = std::fs::symlink_metadata(workspace) else {
        return;
    };
    if metadata_is_link_like(&metadata) || !metadata.is_dir() {
        return;
    }
    if let (Ok(workspace_root), Some(parent)) = (workspace.canonicalize(), workspace.parent())
        && let Ok(parent_root) = parent.canonicalize()
        && workspace_root.starts_with(&parent_root)
        && workspace_root != parent_root
    {
        let _ = std::fs::remove_dir_all(workspace);
        if let Ok(mut entries) = std::fs::read_dir(parent)
            && entries.next().is_none()
        {
            let _ = std::fs::remove_dir(parent);
        }
    }
}

async fn create_session_from_tool(state: &Arc<AppState>, args: &Value) -> Result<String, String> {
    let session_id = generate_available_session_id_for_tool(state).await?;
    let name = tool_args_string(args, "name").unwrap_or_else(|| format!("Session {session_id}"));
    let name = crate::validate_session_display_name(&name)?;
    let purpose = tool_args_string(args, "purpose")
        .map(|value| sanitize_profile_input("purpose", &value, CREATE_SESSION_PROFILE_MAX_CHARS))
        .transpose()?
        .filter(|value| !value.is_empty());
    let identity_profile = tool_args_string(args, "identity_profile")
        .map(|value| {
            sanitize_profile_input("identity_profile", &value, CREATE_SESSION_PROFILE_MAX_CHARS)
        })
        .transpose()?
        .filter(|value| !value.is_empty());
    let user_profile = tool_args_string(args, "user_profile")
        .map(|value| {
            sanitize_profile_input("user_profile", &value, CREATE_SESSION_PROFILE_MAX_CHARS)
        })
        .transpose()?
        .filter(|value| !value.is_empty());
    let style_profile = tool_args_string(args, "style_profile")
        .map(|value| {
            sanitize_profile_input("style_profile", &value, CREATE_SESSION_PROFILE_MAX_CHARS)
        })
        .transpose()?
        .filter(|value| !value.is_empty());
    let agent_notes = tool_args_string(args, "agent_notes")
        .map(|value| {
            sanitize_profile_input("agent_notes", &value, CREATE_SESSION_AGENT_NOTES_MAX_CHARS)
        })
        .transpose()?
        .filter(|value| !value.is_empty());

    let persist_gate = session_store::session_persist_gate(&session_id);
    let _persist_guard = persist_gate.lock().await;
    {
        let sessions = state.sessions.lock().await;
        if crate::find_loaded_session_id(&sessions, &session_id).is_some() {
            return Err("session_control error: generated session id already exists".to_string());
        }
    }
    if session_store::canonical_saved_session_id_result(&session_id)?.is_some() {
        return Err("session_control error: generated session id already exists".to_string());
    }

    let mut session = Session::new_with_id(&session_id, &name);
    if let Err(error) = initialize_created_session_profile(
        &session.workspace,
        &name,
        purpose.as_deref(),
        identity_profile.as_deref(),
        user_profile.as_deref(),
        style_profile.as_deref(),
        agent_notes.as_deref(),
    ) {
        cleanup_failed_created_session(&session_id, &session.workspace);
        return Err(error);
    }

    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    let sys = crate::prompts::build_system_prompt(
        &config,
        &session.workspace,
        &model,
        &session.enabled_system_skills,
    );
    session.messages.push(sys);
    session.updated_at = now_epoch();
    if let Err(error) = session_store::save_session_to_disk_locked(&session).await {
        cleanup_failed_created_session(&session_id, &session.workspace);
        return Err(error);
    }

    {
        let mut sessions = state.sessions.lock().await;
        sessions.insert(session_id.clone(), session.clone());
    }
    crate::broadcast_session_list_payload(state).await;

    let profile = summarize_session_profile(&session.workspace);
    let (agent_summary, user_summary) = session_summary_from_profile(&profile);
    Ok(format!(
        "Created session {} ({}) model={} status=idle skills={} mcp_tools={} task_plan_global={} updated_at={}\n  agent: {}\n  user: {}",
        session.id,
        session.name,
        model,
        enabled_skills_for_session(&session).len(),
        enabled_mcp_tools_for_session(&config, &session).len(),
        config.enable_task_plan,
        session.updated_at,
        agent_summary,
        user_summary
    ))
}

fn tool_args_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn validate_tool_array_len(value: &Value, key: &str, max_items: usize) -> Result<(), String> {
    let Some(value) = value.get(key) else {
        return Ok(());
    };
    let Some(items) = value.as_array() else {
        return Err(format!("session_control error: {key} must be an array"));
    };
    if items.len() > max_items {
        return Err(format!(
            "session_control error: {key} exceeds {max_items} item(s)"
        ));
    }
    if items.iter().any(|item| !item.is_string()) {
        return Err(format!(
            "session_control error: {key} must contain only strings"
        ));
    }
    Ok(())
}

fn tool_args_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn tool_args_bounded_string(value: &Value, key: &str, max_chars: usize) -> Result<String, String> {
    let text = tool_args_string(value, key)
        .ok_or_else(|| format!("session_control error: {key} is required"))?;
    if text.chars().count() > max_chars {
        return Err(format!(
            "session_control error: {key} exceeds {max_chars} characters"
        ));
    }
    Ok(text)
}

fn validate_message_len(message: &str) -> Result<(), String> {
    if message.chars().count() > SESSION_CONTROL_MESSAGE_MAX_CHARS {
        return Err(format!(
            "session_control error: message exceeds {} characters",
            SESSION_CONTROL_MESSAGE_MAX_CHARS
        ));
    }
    Ok(())
}

/// Shared `/delete` safety model and deletion routine used by both the `/delete` slash
/// command (`handle_delete_command` in `commands.rs`) and `session_control.delete_session`.
/// Rejects the caller's own session, the main session, an active connected session, a
/// running session, or any session with active/queued delegated work (direct runs,
/// in-flight group runs, or saved group runs), then removes the persisted state and the
/// default workspace directory. Returns the success message; callers are responsible for
/// broadcasting the updated session list.
pub(crate) async fn delete_session_with_safety_checks(
    state: &AppState,
    target: &str,
    reject_current: Option<&str>,
) -> Result<String, String> {
    let target_session_id = runtime_loop::resolve_session_target_for_delete(state, target).await?;
    if let Some(current) = reject_current
        && target_session_id == current
    {
        return Err("Cannot delete the current session.".to_string());
    }
    if crate::is_main(&target_session_id) {
        return Err("Cannot delete the default main session.".to_string());
    }
    if state
        .active_connections
        .lock()
        .await
        .contains_key(&target_session_id)
    {
        return Err(format!("Cannot delete active session: {target_session_id}"));
    }
    if state
        .active_runs
        .lock()
        .await
        .contains_key(&target_session_id)
    {
        return Err(format!(
            "Cannot delete running session: {target_session_id}"
        ));
    }
    if session_has_active_delegated_work(&target_session_id) {
        return Err(format!(
            "Cannot delete running session: {target_session_id}"
        ));
    }

    // Hold the target's session-control lock across the final delegated-work re-check
    // and the destructive operations. `run_target_run` takes this same lock before it
    // persists an orphan session via `append_target_session_message`, so serializing
    // here closes the TOCTOU window where a group/direct dispatch registers a run after
    // the checks above pass. Lock order matches `run_target_run` (session_control_lock
    // first, group_persist_gate later), so the group prune below cannot deadlock.
    let target_lock = session_control_lock(state, &target_session_id).await;
    let _target_guard = target_lock.lock().await;
    // Re-check under the lock: a run that started between the initial checks and
    // acquiring the lock is now visible (its queued/running state is registered).
    if state
        .active_runs
        .lock()
        .await
        .contains_key(&target_session_id)
    {
        return Err(format!(
            "Cannot delete running session: {target_session_id}"
        ));
    }
    if session_has_active_delegated_work(&target_session_id) {
        return Err(format!(
            "Cannot delete running session: {target_session_id}"
        ));
    }

    // Session writers (including `/think`, the Composer model picker and
    // config-reload Effort normalization) all serialize through this gate.
    // Hold it until both SQLite and the in-memory map no longer contain the
    // Session, so an in-flight preference write cannot resurrect a deleted
    // row and a late writer observes `session_not_found` instead.
    let persist_gate = session_store::session_persist_gate(&target_session_id);
    let session_persist_guard = persist_gate.lock().await;

    let roster_gate = session_group::group_roster_gate();
    let roster_guard = roster_gate.lock().await;
    #[cfg(not(test))]
    let mut affected_group_ids = {
        let mut affected_group_ids = Vec::new();
        for summary in session_group::list_saved_group_summaries_result()
            .map_err(|_| storage_protected_control_error())?
        {
            if summary.corrupt {
                continue;
            }
            let Some(group) = session_group::load_group_from_storage_result(&summary.id)
                .map_err(|_| storage_protected_control_error())?
            else {
                continue;
            };
            let references = group
                .members
                .iter()
                .any(|member| member == &target_session_id)
                || group.admins.iter().any(|admin| admin == &target_session_id)
                || group.pending_votes.iter().any(|vote| {
                    vote.target_session_id == target_session_id.as_str()
                        || vote.requester_session_id == target_session_id.as_str()
                        || vote
                            .approvals
                            .iter()
                            .any(|approval| approval == &target_session_id)
                });
            if references {
                affected_group_ids.push(summary.id);
            }
        }
        affected_group_ids
    };
    let workspace_root = session_store::session_workspace_root_for_delete(&target_session_id)?;
    #[cfg(not(test))]
    let group_guards = {
        let mut affected_group_ids = affected_group_ids.clone();
        affected_group_ids.sort();
        affected_group_ids.dedup();
        let mut guards = Vec::with_capacity(affected_group_ids.len());
        for group_id in affected_group_ids {
            guards.push(
                session_group::group_persist_gate(&group_id)
                    .lock_owned()
                    .await,
            );
        }
        guards
    };
    // Remove the persisted session and the in-memory entry before the workspace directory.
    // The session list is built from SQLite plus the in-memory map, so doing
    // this first means a later workspace-removal failure can no longer leave a session
    // that is still listed but has no workspace; at worst an orphan workspace dir remains.
    let delete_outcome = session_store::delete_session_from_storage(&target_session_id)
        .await
        .map_err(|error| format!("Failed to delete session {target_session_id}: {error}"))?;
    let _persisted_deleted = delete_outcome.deleted;
    #[cfg(not(test))]
    {
        affected_group_ids.extend(delete_outcome.affected_group_ids);
        affected_group_ids.sort();
        affected_group_ids.dedup();
    }
    let removed = {
        let mut sessions = state.sessions.lock().await;
        sessions.remove(&target_session_id).is_some()
    };
    drop(session_persist_guard);
    #[cfg(not(test))]
    let mut group_notifications_incomplete = false;
    #[cfg(test)]
    let group_notifications_incomplete = false;
    #[cfg(test)]
    prune_session_from_all_groups(state, &target_session_id).await;
    #[cfg(not(test))]
    if !affected_group_ids.is_empty() {
        for group_id in &affected_group_ids {
            match session_group::load_group_from_storage_result(group_id) {
                Ok(Some(group)) => send_group_info(state, &group).await,
                Ok(None) => {}
                Err(error) => {
                    group_notifications_incomplete = true;
                    eprintln!(
                        "WARNING: Session {target_session_id} was deleted, but Group {group_id} could not be refreshed: {error}"
                    );
                }
            }
        }
        crate::broadcast_group_list_payload(state).await;
    }
    #[cfg(not(test))]
    drop(group_guards);
    drop(roster_guard);
    let workspace_cleanup_warning = match tokio::fs::try_exists(&workspace_root).await {
        Ok(true) => tokio::fs::remove_dir_all(&workspace_root)
            .await
            .err()
            .map(|error| {
                format!("session workspace cleanup was incomplete for {target_session_id}: {error}")
            }),
        Ok(false) => None,
        Err(error) => Some(format!(
            "session workspace cleanup could not inspect {target_session_id}: {error}"
        )),
    };
    let mut message = if removed {
        format!("Deleted session: {target_session_id}")
    } else {
        format!("Deleted saved session: {target_session_id}")
    };
    if let Some(warning) = workspace_cleanup_warning {
        eprintln!("WARNING: {warning}");
        message.push_str("\nWarning: ");
        message.push_str(&warning);
    }
    if group_notifications_incomplete {
        message.push_str(
            "\nWarning: Some Group notifications could not be refreshed. Repair local storage and restart LingClaw.",
        );
    }
    Ok(message)
}

/// True when the session has active or queued delegated work that must be stopped before
/// the session can be safely deleted: an active direct run, an in-flight group run, or a
/// queued/running run recorded in a persisted group.
fn session_has_active_delegated_work(session_id: &str) -> bool {
    direct_run_status_for_session(session_id).is_some_and(DirectRunStatus::is_active)
        || active_group_run_statuses_by_session()
            .get(session_id)
            // "unknown" means a control entry exists but the group file could not be read;
            // treat it as active so a likely in-flight run is not deleted out from under.
            .is_some_and(|status| matches!(status.as_str(), "queued" | "running" | "unknown"))
        || saved_group_has_active_run_for_session(session_id)
}

async fn delete_session_from_control(
    state: &AppState,
    target_session_id: &str,
) -> Result<String, String> {
    if crate::is_main(session_store::validate_session_id(target_session_id)?) {
        return Err("session_control error: cannot delete the main session.".to_string());
    }
    let message = delete_session_with_safety_checks(state, target_session_id, None).await?;
    crate::broadcast_session_list_payload(state).await;
    Ok(message)
}

pub(crate) async fn delete_group_from_control(
    state: &AppState,
    group_id: &str,
) -> Result<String, String> {
    let group_id = session_group::validate_group_id(group_id)?.to_string();
    let gate = session_group::group_persist_gate(&group_id);
    let _guard = gate.lock().await;
    let group = session_group::load_group_from_storage_result(&group_id)?
        .ok_or_else(|| format!("Group '{}' not found", group_id))?;
    if session_group::group_has_active_runs(&group) {
        return Err(format!(
            "Group '{}' has queued or running session runs. Stop them before deleting the group.",
            group_id
        ));
    }
    session_group::delete_group_from_disk_locked(&group_id).await?;
    crate::close_group_client(state, &group_id).await;
    crate::broadcast_group_list_payload(state).await;
    Ok(format!("Deleted group: {group_id}"))
}

fn group_admin_vote_threshold(admin_count: usize) -> usize {
    (admin_count * 2).div_ceil(3)
}

fn stop_active_group_runs_for_member(
    group: &mut SessionGroup,
    target_session_id: &str,
    now: u64,
    run_ids: &mut Vec<String>,
    updated_runs: &mut Vec<GroupRun>,
) {
    for run in &mut group.runs {
        if run.session_id == target_session_id
            && session_group::is_active_group_run_status(&run.status)
            && let Some(updated) =
                apply_group_run_status_transition(run, "stopped", None, None, now)
        {
            run_ids.push(updated.id.clone());
            updated_runs.push(updated);
        }
    }
}

fn apply_group_member_removal_locked(
    group: &mut SessionGroup,
    target_session_id: &str,
    now: u64,
    run_ids: &mut Vec<String>,
    updated_runs: &mut Vec<GroupRun>,
) -> bool {
    if !group
        .members
        .iter()
        .any(|member| member == target_session_id)
    {
        return false;
    }
    stop_active_group_runs_for_member(group, target_session_id, now, run_ids, updated_runs);
    remove_member_from_group(group, target_session_id);
    true
}

fn reconcile_pending_group_votes_locked(
    group: &mut SessionGroup,
    now: u64,
    run_ids: &mut Vec<String>,
    updated_runs: &mut Vec<GroupRun>,
) -> Vec<String> {
    let mut removed = Vec::new();
    // Capture the approval threshold once for the whole settlement pass. Removing an
    // admin-target inside the loop shrinks `group.admins`; recomputing the threshold
    // each iteration would retroactively lower the bar for other pending votes that
    // were cast under the original (larger) admin count, removing a member who never
    // reached the required 2/3-of-admins approval.
    session_group::normalize_group(group);
    let threshold = group_admin_vote_threshold(group.admins.len()).max(1);
    loop {
        session_group::normalize_group(group);
        let member_set = group.members.iter().cloned().collect::<HashSet<_>>();
        let admins = group.admins.clone();
        let mut achieved = Vec::new();
        let mut retained = Vec::new();
        for mut vote in std::mem::take(&mut group.pending_votes) {
            if vote.action != "remove_member" || !member_set.contains(&vote.target_session_id) {
                continue;
            }
            vote.approvals = session_group::normalize_vote_approvals(vote.approvals, &admins);
            if vote.approvals.is_empty() {
                continue;
            }
            vote.threshold = threshold;
            vote.updated_at = now;
            if vote.approvals.len() >= vote.threshold {
                if !achieved
                    .iter()
                    .any(|target| target == &vote.target_session_id)
                {
                    achieved.push(vote.target_session_id);
                }
            } else {
                retained.push(vote);
            }
        }
        group.pending_votes = retained;
        if achieved.is_empty() {
            break;
        }
        for target in achieved {
            if apply_group_member_removal_locked(group, &target, now, run_ids, updated_runs) {
                removed.push(target);
            }
        }
    }
    removed
}

pub(crate) async fn promote_group_admin_from_control(
    state: &AppState,
    group_id: &str,
    target_session_id: &str,
) -> Result<(String, SessionGroup), String> {
    let target_session_id = session_store::validate_session_id(target_session_id)?.to_string();
    if crate::is_main(&target_session_id) {
        return Err("session_control error: main is already the group owner.".to_string());
    }
    let group_id = session_group::validate_group_id(group_id)?.to_string();
    let (run_ids, updated_runs, group) = {
        let gate = session_group::group_persist_gate(&group_id);
        let _guard = gate.lock().await;
        let mut group = session_group::load_group_from_storage_result(&group_id)?
            .ok_or_else(|| format!("Group '{}' not found", group_id))?;
        if !group
            .members
            .iter()
            .any(|member| member == &target_session_id)
        {
            return Err(format!(
                "Target session '{}' is not a member of group {}.",
                target_session_id, group_id
            ));
        }
        if !group.admins.iter().any(|admin| admin == &target_session_id) {
            group.admins.push(target_session_id.clone());
            group.admins =
                session_group::normalize_admins(std::mem::take(&mut group.admins), &group.members);
        }
        let now = now_epoch();
        let mut run_ids = Vec::new();
        let mut updated_runs = Vec::new();
        reconcile_pending_group_votes_locked(&mut group, now, &mut run_ids, &mut updated_runs);
        group.updated_at = now;
        session_group::save_group_to_disk_locked(&group).await?;
        (run_ids, updated_runs, group)
    };
    stop_group_run_controls(&group_id, &run_ids);
    for run in &updated_runs {
        emit_group_run_status_events(state, &group_id, run).await;
    }
    send_group_info(state, &group).await;
    crate::broadcast_group_list_payload(state).await;
    Ok((
        format!(
            "Promoted {} to group admin in {}.",
            target_session_id, group_id
        ),
        group,
    ))
}

fn remove_member_from_group(group: &mut SessionGroup, target_session_id: &str) {
    group.members.retain(|member| member != target_session_id);
    group.admins.retain(|admin| admin != target_session_id);
    group
        .pending_votes
        .retain(|vote| vote.target_session_id != target_session_id);
}

/// Remove a now-deleted session id from every saved group's members/admins/pending_votes
/// so it cannot become a ghost member. Each group is mutated under its own persist gate via
/// `mutate_group_result`; groups that fail to load or no longer reference the id are left
/// untouched. Failures are logged, never propagated: the session is already deleted and a
/// stale group reference must not turn a successful delete into an error.
#[cfg(test)]
async fn prune_session_from_all_groups(state: &AppState, deleted_session_id: &str) {
    // `main` is never a group member and is never deletable, so this only runs for workers.
    let mut pruned_any = false;
    for summary in session_group::list_saved_group_summaries() {
        if summary.corrupt {
            continue;
        }
        // Cheap pre-filter: only touch (and re-save) groups that actually reference the id.
        let references = session_group::load_group_from_disk(&summary.id).is_some_and(|group| {
            group.members.iter().any(|m| m == deleted_session_id)
                || group.admins.iter().any(|a| a == deleted_session_id)
                || group
                    .pending_votes
                    .iter()
                    .any(|vote| vote.target_session_id == deleted_session_id)
        });
        if !references {
            continue;
        }
        match mutate_group_result(&summary.id, |group| {
            remove_member_from_group(group, deleted_session_id);
            session_group::normalize_group(group);
            Ok::<(), String>(())
        })
        .await
        {
            Ok((group, ())) => {
                send_group_info(state, &group).await;
                pruned_any = true;
            }
            Err(error) => {
                eprintln!(
                    "WARN: failed to prune deleted session {deleted_session_id} from group {}: {error}",
                    summary.id
                );
            }
        }
    }
    // Only refresh the group list when something actually changed, so a delete that
    // touches no group emits no spurious broadcast.
    if pruned_any {
        crate::broadcast_group_list_payload(state).await;
    }
}

pub(crate) async fn remove_group_member_direct(
    state: &AppState,
    group_id: &str,
    target_session_id: &str,
) -> Result<(String, SessionGroup), String> {
    let target_session_id = session_store::validate_session_id(target_session_id)?.to_string();
    if crate::is_main(&target_session_id) {
        return Err("session_control error: cannot remove the main group owner.".to_string());
    }
    let group_id = session_group::validate_group_id(group_id)?.to_string();
    let (run_ids, updated_runs, group) = {
        let gate = session_group::group_persist_gate(&group_id);
        let _guard = gate.lock().await;
        let mut group = session_group::load_group_from_storage_result(&group_id)?
            .ok_or_else(|| format!("Group '{}' not found", group_id))?;
        if !group
            .members
            .iter()
            .any(|member| member == &target_session_id)
        {
            return Err(format!(
                "Target session '{}' is not a member of group {}.",
                target_session_id, group_id
            ));
        }
        let now = now_epoch();
        let mut run_ids = Vec::new();
        let mut updated_runs = Vec::new();
        apply_group_member_removal_locked(
            &mut group,
            &target_session_id,
            now,
            &mut run_ids,
            &mut updated_runs,
        );
        reconcile_pending_group_votes_locked(&mut group, now, &mut run_ids, &mut updated_runs);
        group.updated_at = now;
        session_group::save_group_to_disk_locked(&group).await?;
        (run_ids, updated_runs, group)
    };
    stop_group_run_controls(&group_id, &run_ids);
    for run in &updated_runs {
        emit_group_run_status_events(state, &group_id, run).await;
    }
    send_group_info(state, &group).await;
    crate::send_group_client_event(
        state,
        &group.id,
        GroupClientEvent::reliable(json!({
            "type":"system",
            "content": format!("Removed {target_session_id} from group {group_id}."),
            "dismissible": true
        })),
    )
    .await;
    crate::broadcast_group_list_payload(state).await;
    Ok((
        format!("Removed {} from group {}.", target_session_id, group_id),
        group,
    ))
}

async fn request_group_member_removal_vote(
    state: &AppState,
    group_id: &str,
    requester_session_id: &str,
    target_session_id: &str,
) -> Result<String, String> {
    let requester_session_id =
        session_store::validate_session_id(requester_session_id)?.to_string();
    let target_session_id = session_store::validate_session_id(target_session_id)?.to_string();
    if crate::is_main(&target_session_id) {
        return Err("session_control error: cannot remove the main group owner.".to_string());
    }
    let group_id = session_group::validate_group_id(group_id)?.to_string();
    let mut removed_by_vote = false;
    let mut approval_count: usize;
    let mut threshold_count: usize;
    let (run_ids, updated_runs, group) = {
        let gate = session_group::group_persist_gate(&group_id);
        let _guard = gate.lock().await;
        let mut group = session_group::load_group_from_storage_result(&group_id)?
            .ok_or_else(|| format!("Group '{}' not found", group_id))?;
        if !group
            .admins
            .iter()
            .any(|admin| admin == &requester_session_id)
        {
            return Err(format!(
                "Requester '{}' is not a promoted admin of group {}.",
                requester_session_id, group_id
            ));
        }
        if !group
            .members
            .iter()
            .any(|member| member == &target_session_id)
        {
            return Err(format!(
                "Target session '{}' is not a member of group {}.",
                target_session_id, group_id
            ));
        }
        let threshold = group_admin_vote_threshold(group.admins.len()).max(1);
        threshold_count = threshold;
        let now = now_epoch();
        let mut run_ids = Vec::new();
        let mut updated_runs = Vec::new();
        if threshold <= 1 {
            approval_count = 1;
            removed_by_vote = apply_group_member_removal_locked(
                &mut group,
                &target_session_id,
                now,
                &mut run_ids,
                &mut updated_runs,
            );
            // The unconditional reconcile after this if/else (below) settles any
            // remaining pending votes for every branch, so a second call here is
            // redundant: the direct removal already set `removed_by_vote`.
        } else if let Some(vote) = group.pending_votes.iter_mut().find(|vote| {
            vote.action == "remove_member" && vote.target_session_id == target_session_id
        }) {
            vote.threshold = threshold;
            if !vote
                .approvals
                .iter()
                .any(|approval| approval == &requester_session_id)
            {
                vote.approvals.push(requester_session_id.clone());
            }
            vote.updated_at = now;
            approval_count = vote.approvals.len();
        } else {
            let vote_id = next_id("gvote");
            group.pending_votes.push(session_group::GroupVote {
                id: vote_id,
                action: "remove_member".to_string(),
                target_session_id: target_session_id.clone(),
                requester_session_id: requester_session_id.clone(),
                approvals: vec![requester_session_id.clone()],
                threshold,
                created_at: now,
                updated_at: now,
            });
            approval_count = 1;
        }
        let removed =
            reconcile_pending_group_votes_locked(&mut group, now, &mut run_ids, &mut updated_runs);
        removed_by_vote =
            removed_by_vote || removed.iter().any(|target| target == &target_session_id);
        if !removed_by_vote
            && let Some(vote) = group.pending_votes.iter().find(|vote| {
                vote.action == "remove_member" && vote.target_session_id == target_session_id
            })
        {
            approval_count = vote.approvals.len();
            threshold_count = vote.threshold;
        }
        group.updated_at = now;
        session_group::save_group_to_disk_locked(&group).await?;
        Ok::<_, String>((run_ids, updated_runs, group))
    }?;
    stop_group_run_controls(&group_id, &run_ids);
    for run in &updated_runs {
        emit_group_run_status_events(state, &group_id, run).await;
    }
    if removed_by_vote {
        send_group_info(state, &group).await;
        crate::send_group_client_event(
            state,
            &group.id,
            GroupClientEvent::reliable(json!({
                "type":"system",
                "content": format!("Removed {target_session_id} from group {group_id}."),
                "dismissible": true
            })),
        )
        .await;
        crate::broadcast_group_list_payload(state).await;
        return Ok(format!(
            "Removed {} from group {}.",
            target_session_id, group_id
        ));
    }
    send_group_info(state, &group).await;
    crate::send_group_client_event(
        state,
        &group.id,
        GroupClientEvent::reliable(json!({
            "type":"system",
            "content": format!(
                "Removal vote for {} has {}/{} promoted admin approval(s).",
                target_session_id, approval_count, threshold_count
            ),
            "dismissible": true
        })),
    )
    .await;
    Ok(format!(
        "Removal vote opened for {} in group {}.",
        target_session_id, group_id
    ))
}

async fn delete_session_from_tool(state: &AppState, args: &Value) -> Result<String, String> {
    let target = tool_args_string(args, "target")
        .or_else(|| tool_args_string(args, "session_id"))
        .ok_or_else(|| "session_control error: target is required".to_string())?;
    delete_session_from_control(state, &target).await
}

async fn delete_group_from_tool(state: &AppState, args: &Value) -> Result<String, String> {
    let group_id = tool_args_string(args, "group_id")
        .ok_or_else(|| "session_control error: group_id is required".to_string())?;
    delete_group_from_control(state, &group_id).await
}

async fn promote_group_admin_from_tool(state: &AppState, args: &Value) -> Result<String, String> {
    let group_id = tool_args_string(args, "group_id")
        .ok_or_else(|| "session_control error: group_id is required".to_string())?;
    let target = tool_args_string(args, "target")
        .or_else(|| tool_args_string(args, "session_id"))
        .ok_or_else(|| "session_control error: target is required".to_string())?;
    promote_group_admin_from_control(state, &group_id, &target)
        .await
        .map(|(message, _group)| message)
}

async fn remove_group_member_from_tool(state: &AppState, args: &Value) -> Result<String, String> {
    let group_id = tool_args_string(args, "group_id")
        .ok_or_else(|| "session_control error: group_id is required".to_string())?;
    let target = tool_args_string(args, "target")
        .or_else(|| tool_args_string(args, "session_id"))
        .ok_or_else(|| "session_control error: target is required".to_string())?;
    let requester = tool_args_string(args, "requester_session_id")
        .unwrap_or_else(|| MAIN_SESSION_ID.to_string());
    if crate::is_main(&requester) {
        remove_group_member_direct(state, &group_id, &target)
            .await
            .map(|(message, _group)| message)
    } else {
        request_group_member_removal_vote(state, &group_id, &requester, &target).await
    }
}

async fn create_group_from_tool(state: &AppState, args: &Value) -> Result<String, String> {
    validate_tool_array_len(args, "members", SESSION_CONTROL_MEMBERS_MAX_ITEMS)?;
    let roster_gate = session_group::group_roster_gate();
    let _roster_guard = roster_gate.lock().await;
    let group_id = session_group::generate_available_group_id()?;
    let name = tool_args_string(args, "name").unwrap_or_else(|| format!("Group {group_id}"));
    let name = session_group::validate_group_name(&name)?;
    let members = canonicalize_existing_group_members(tool_args_array(args, "members"))
        .map_err(group_member_resolution_control_error)?;
    let group = SessionGroup::new(&group_id, &name, members);
    let gate = session_group::group_persist_gate(&group_id);
    let _guard = gate.lock().await;
    session_group::save_group_to_disk_locked(&group).await?;
    crate::broadcast_group_list_payload(state).await;
    Ok(format!(
        "Created group {} ({}) with {} member(s).",
        group.id,
        group.name,
        group.members.len()
    ))
}

async fn update_group_from_tool(state: &AppState, args: &Value) -> Result<String, String> {
    validate_tool_array_len(args, "members", SESSION_CONTROL_MEMBERS_MAX_ITEMS)?;
    let group_id = tool_args_string(args, "group_id")
        .ok_or_else(|| "session_control error: group_id is required".to_string())?;
    let name = tool_args_string(args, "name")
        .map(|name| session_group::validate_group_name(&name))
        .transpose()?;
    let members = args
        .get("members")
        .and_then(Value::as_array)
        .map(|_| tool_args_array(args, "members"));
    let roster_gate = session_group::group_roster_gate();
    let _roster_guard = roster_gate.lock().await;
    let members = members
        .map(canonicalize_existing_group_members)
        .transpose()
        .map_err(group_member_resolution_control_error)?;
    let group = mutate_group(&group_id, |group| {
        if let Some(name) = name {
            group.name = name;
        }
        if let Some(members) = members {
            // normalize_group re-normalizes members, filters admins to the new member
            // set, and prunes stale votes, so one normalize pass is sufficient.
            group.members = members;
            session_group::normalize_group(group);
        }
        group.clone()
    })
    .await?
    .1;
    send_group_info(state, &group).await;
    crate::broadcast_group_list_payload(state).await;
    Ok(format!(
        "Updated group {} ({}) with {} member(s).",
        group.id,
        group.name,
        group.members.len()
    ))
}

async fn post_group_message_from_tool(state: &AppState, args: &Value) -> Result<String, String> {
    let group_id = tool_args_string(args, "group_id")
        .ok_or_else(|| "session_control error: group_id is required".to_string())?;
    let message = tool_args_bounded_string(args, "message", SESSION_CONTROL_MESSAGE_MAX_CHARS)?;
    let msg = mutate_group(&group_id, |group| {
        append_group_message(group, "main", None, message.clone(), None, None)
    })
    .await?
    .1;
    crate::send_group_client_event(
        state,
        &group_id,
        GroupClientEvent::reliable(json!({
            "type": "group_message",
            "group_id": group_id,
            "message": msg,
        })),
    )
    .await;
    crate::broadcast_group_list_payload(state).await;
    Ok(format!("Posted group message {} to {}.", msg.id, group_id))
}

async fn dispatch_from_tool(state: &Arc<AppState>, args: &Value) -> Result<String, String> {
    validate_tool_array_len(args, "targets", SESSION_CONTROL_TARGETS_MAX_ITEMS)?;
    let group_id = tool_args_string(args, "group_id");
    let message = tool_args_bounded_string(args, "message", SESSION_CONTROL_MESSAGE_MAX_CHARS)?;
    let run_mode = parse_run_mode(
        args.get("run_mode")
            .and_then(Value::as_str)
            .unwrap_or("execute"),
    )?;
    if run_mode == AgentRunMode::PlanOnly {
        return Err("group_plan_mode_unsupported".to_string());
    }
    let raw_targets = tool_args_array(args, "targets");
    let max_targets = if group_id.is_some() && raw_targets.is_empty() {
        SESSION_CONTROL_MEMBERS_MAX_ITEMS
    } else {
        SESSION_CONTROL_TARGETS_MAX_ITEMS
    };
    let targets =
        prepare_dispatch_targets(state, group_id.as_deref(), raw_targets, max_targets).await?;
    ensure_explicit_target_models(state, &targets).await?;
    let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let summary_budget = args
        .get("summary_budget")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(4_000)
        .clamp(500, 20_000);
    let group_message = group_id.as_ref().map(|_| DispatchGroupMessage {
        role: "main".to_string(),
        session_id: None,
        turn_id: None,
    });
    let runs = dispatch_to_sessions(
        state,
        DispatchRequest {
            group_id,
            targets,
            optional_targets: HashSet::new(),
            message,
            group_message,
            run_mode,
            wait,
            summary_budget,
            mention_depth: 0,
        },
    )
    .await?;
    Ok(format!(
        "Dispatched {} session run(s): {}",
        runs.len(),
        runs.iter()
            .map(|run| format!("{}:{}", run.session_id, run.run_id))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

async fn stop_group_runs(
    state: &AppState,
    group_id: &str,
    mut targets: Vec<String>,
) -> Result<String, String> {
    let group_id = session_group::validate_group_id(group_id)?.to_string();
    let (run_ids, updated_runs) = {
        let gate = session_group::group_persist_gate(&group_id);
        let _guard = gate.lock().await;
        let mut group = session_group::load_group_from_storage_result(&group_id)?
            .ok_or_else(|| format!("Group '{}' not found", group_id))?;
        if targets.is_empty() {
            targets = group
                .runs
                .iter()
                .filter(|run| matches!(run.status.as_str(), "queued" | "running"))
                .map(|run| run.session_id.clone())
                .collect();
        }
        targets = normalize_target_ids(targets);
        if targets.is_empty() {
            return Err("session_control error: no running target sessions selected".to_string());
        }
        let target_set = targets.iter().cloned().collect::<HashSet<_>>();
        let now = now_epoch();
        let mut run_ids = Vec::new();
        let mut updated_runs = Vec::new();
        for run in &mut group.runs {
            if !target_set_contains_session(&target_set, &run.session_id)
                || !matches!(run.status.as_str(), "queued" | "running")
            {
                continue;
            }
            if let Some(updated) =
                apply_group_run_status_transition(run, "stopped", None, None, now)
            {
                run_ids.push(updated.id.clone());
                updated_runs.push(updated);
            }
        }
        if !updated_runs.is_empty() {
            group.updated_at = now;
            session_group::save_group_to_disk_locked(&group).await?;
        }
        (run_ids, updated_runs)
    };
    stop_group_run_controls(&group_id, &run_ids);
    for run in &updated_runs {
        emit_group_run_status_events(state, &group_id, run).await;
    }
    Ok(format!(
        "Stop requested for {} group run(s) and 0 direct run(s).",
        updated_runs.len()
    ))
}

async fn stop_from_tool(state: &AppState, args: &Value) -> Result<String, String> {
    validate_tool_array_len(args, "targets", SESSION_CONTROL_TARGETS_MAX_ITEMS)?;
    let group_id = tool_args_string(args, "group_id");
    let targets = tool_args_array(args, "targets");
    if let Some(group_id) = group_id.as_deref() {
        stop_group_runs(state, group_id, targets).await
    } else {
        let targets = normalize_target_ids(targets);
        if targets.is_empty() {
            return Err("session_control error: no running target sessions selected".to_string());
        }
        let stopped_direct_runs = stop_direct_runs_for_targets(&targets);
        Ok(format!(
            "Stop requested for 0 group run(s) and {stopped_direct_runs} direct run(s)."
        ))
    }
}

pub(crate) async fn execute_session_control_tool(
    state: &Arc<AppState>,
    current_session_id: &str,
    args_str: &str,
) -> crate::tools::ToolOutcome {
    if !crate::is_main(current_session_id) {
        return crate::tools::ToolOutcome {
            output: "session_control error: this tool is only available in the main session."
                .to_string(),
            is_error: true,
            duration_ms: 0,
            subagent_snapshot: None,
            images: Vec::new(),
        };
    }
    let started = std::time::Instant::now();
    let args: Value = match serde_json::from_str(args_str) {
        Ok(args) => args,
        Err(error) => {
            return crate::tools::ToolOutcome {
                output: format!("session_control error: invalid arguments JSON: {error}"),
                is_error: true,
                duration_ms: started.elapsed().as_millis() as u64,
                subagent_snapshot: None,
                images: Vec::new(),
            };
        }
    };
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match action {
        "list_sessions" => session_list_output(state).await,
        "create_session" => create_session_from_tool(state, &args).await,
        "delete_session" => delete_session_from_tool(state, &args).await,
        "describe_session" => describe_session_from_tool(state, &args).await,
        "list_groups" => group_list_output(),
        "create_group" => create_group_from_tool(state, &args).await,
        "update_group" => update_group_from_tool(state, &args).await,
        "delete_group" => delete_group_from_tool(state, &args).await,
        "promote_group_admin" => promote_group_admin_from_tool(state, &args).await,
        "remove_group_member" => remove_group_member_from_tool(state, &args).await,
        "post_group_message" => post_group_message_from_tool(state, &args).await,
        "dispatch" => dispatch_from_tool(state, &args).await,
        "collect" => {
            let group_id = tool_args_string(&args, "group_id")
                .ok_or_else(|| "session_control error: group_id is required".to_string());
            match group_id {
                Ok(group_id) => {
                    session_group::load_group_from_storage_result(&group_id).and_then(|group| {
                        group
                            .map(|group| collect_group_summary(&group))
                            .ok_or_else(|| format!("Group '{}' not found", group_id))
                    })
                }
                Err(error) => Err(error),
            }
        }
        "stop" => stop_from_tool(state, &args).await,
        _ => Err("session_control error: unknown action".to_string()),
    };
    crate::tools::ToolOutcome {
        output: match &result {
            Ok(output) => output.clone(),
            Err(error) => error.clone(),
        },
        is_error: result.is_err(),
        duration_ms: started.elapsed().as_millis() as u64,
        subagent_snapshot: None,
        images: Vec::new(),
    }
}

#[derive(Deserialize)]
pub(crate) struct GroupQuery {
    #[serde(default)]
    pub(crate) group: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct GroupMemberQuery {
    #[serde(default)]
    pub(crate) group: Option<String>,
    #[serde(default)]
    pub(crate) session: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionGroupRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    /// Absent (`None`) means "leave members unchanged" on update. A name-only
    /// PUT must not wipe the existing roster, so this is `Option` rather than a
    /// bare `Vec` that would default to empty.
    #[serde(default)]
    pub(crate) members: Option<Vec<String>>,
}

pub(crate) async fn api_session_groups(
    State(_state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let groups = session_group::list_saved_group_summaries_result()
        .map_err(|_| crate::storage_protected_api_error())?;
    Ok(Json(json!({
        "groups": groups
            .into_iter()
            .map(|summary| summary.to_json())
            .collect::<Vec<_>>()
    })))
}

async fn generate_available_group_id() -> Result<String, (StatusCode, Json<serde_json::Value>)> {
    session_group::generate_available_group_id().map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": error })),
        )
    })
}

#[derive(Debug)]
enum GroupMemberResolutionError {
    Missing(Vec<String>),
    Storage(String),
}

fn canonicalize_existing_group_members(
    members: Vec<String>,
) -> Result<Vec<String>, GroupMemberResolutionError> {
    let members = session_group::normalize_members(members);
    let mut canonical_members = Vec::with_capacity(members.len());
    let mut missing_members = Vec::new();
    let mut seen = HashSet::new();
    for member in members {
        match session_store::canonical_saved_session_id_result(&member) {
            Ok(Some(canonical)) => {
                let key = if cfg!(windows) {
                    canonical.to_ascii_lowercase()
                } else {
                    canonical.clone()
                };
                if seen.insert(key) {
                    canonical_members.push(canonical);
                }
            }
            Ok(None) => missing_members.push(member),
            Err(error) => return Err(GroupMemberResolutionError::Storage(error)),
        }
    }
    if missing_members.is_empty() {
        Ok(canonical_members)
    } else {
        Err(GroupMemberResolutionError::Missing(missing_members))
    }
}

fn group_member_resolution_control_error(error: GroupMemberResolutionError) -> String {
    match error {
        GroupMemberResolutionError::Missing(members) => format!(
            "session_control error: unknown group member session(s): {}",
            members.join(", ")
        ),
        GroupMemberResolutionError::Storage(error) => {
            format!("session_control error: failed to validate group members: {error}")
        }
    }
}

fn canonicalize_requested_group_members(
    members: Vec<String>,
) -> Result<Vec<String>, (StatusCode, Json<serde_json::Value>)> {
    canonicalize_existing_group_members(members).map_err(|error| match error {
        GroupMemberResolutionError::Missing(missing_members) => (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "Unknown group member session(s): {}",
                    missing_members.join(", ")
                )
            })),
        ),
        GroupMemberResolutionError::Storage(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to validate group members: {error}") })),
        ),
    })
}

fn group_save_api_error(error: String) -> (StatusCode, Json<serde_json::Value>) {
    let status = if error.starts_with(crate::storage::GROUP_MISSING_SESSIONS_ERROR_PREFIX) {
        StatusCode::CONFLICT
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    (
        status,
        Json(json!({ "error": format!("Failed to save group: {error}") })),
    )
}

pub(crate) async fn api_post_session_group(
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(request): Json<SessionGroupRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    crate::validate_local_request_headers(&headers)?;

    let roster_gate = session_group::group_roster_gate();
    let _roster_guard = roster_gate.lock().await;
    let group_id = generate_available_group_id().await?;
    let name = match request.name.as_deref() {
        Some(name) => session_group::validate_group_name(name)
            .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?,
        None => format!("Group {group_id}"),
    };
    let members = canonicalize_requested_group_members(request.members.unwrap_or_default())?;
    let group = session_group::SessionGroup::new(&group_id, &name, members);
    let gate = session_group::group_persist_gate(&group_id);
    let _guard = gate.lock().await;
    session_group::save_group_to_disk_locked(&group)
        .await
        .map_err(group_save_api_error)?;

    crate::broadcast_group_list_payload(&state).await;
    Ok(Json(json!({
        "ok": true,
        "group": session_group::SessionGroupSummary::from_group(&group).to_json(),
    })))
}

pub(crate) async fn api_get_session_group(
    Query(query): Query<GroupQuery>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let group_id = query.group.as_deref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing group id" })),
        )
    })?;
    let group_id = session_group::validate_group_id(group_id)
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
    let group = session_group::load_group_from_storage_result(group_id)
        .map_err(|_| crate::storage_protected_api_error())?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Group '{}' not found", group_id) })),
            )
        })?;
    Ok(Json(json!({"group": group_json(&state, &group).await})))
}

pub(crate) async fn api_put_session_group(
    Query(query): Query<GroupQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
    Json(request): Json<SessionGroupRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    crate::validate_local_request_headers(&headers)?;

    let roster_gate = session_group::group_roster_gate();
    let _roster_guard = roster_gate.lock().await;
    let group_id = query.group.as_deref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing group id" })),
        )
    })?;
    let group_id = session_group::validate_group_id(group_id)
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?
        .to_string();
    let gate = session_group::group_persist_gate(&group_id);
    let _guard = gate.lock().await;
    let mut group = session_group::load_group_from_storage_result(&group_id)
        .map_err(|_| crate::storage_protected_api_error())?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Group '{}' not found", group_id) })),
            )
        })?;
    if let Some(name) = request.name.as_deref() {
        group.name = session_group::validate_group_name(name)
            .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
    }
    // A name-only PUT (members omitted) must preserve the existing roster, so we
    // only replace members when the request actually supplies them. normalize_group
    // re-normalizes members, filters admins to the member set, prunes stale votes,
    // and bumps the version; it is idempotent on the already-normalized members we
    // loaded from disk, so running it once is sufficient either way.
    if let Some(members) = request.members {
        group.members = canonicalize_requested_group_members(members)?;
    }
    session_group::normalize_group(&mut group);
    group.updated_at = now_epoch();
    session_group::save_group_to_disk_locked(&group)
        .await
        .map_err(group_save_api_error)?;

    send_group_info(&state, &group).await;
    crate::broadcast_group_list_payload(&state).await;
    Ok(Json(json!({
        "ok": true,
        "group": session_group::SessionGroupSummary::from_group(&group).to_json(),
    })))
}

pub(crate) async fn api_delete_session_group(
    Query(query): Query<GroupQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    crate::validate_local_request_headers(&headers)?;

    let group_id = query.group.as_deref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing group id" })),
        )
    })?;
    let group_id = session_group::validate_group_id(group_id)
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?
        .to_string();
    let gate = session_group::group_persist_gate(&group_id);
    let _guard = gate.lock().await;
    let group = session_group::load_group_from_storage_result(&group_id)
        .map_err(|_| crate::storage_protected_api_error())?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("Group '{}' not found", group_id) })),
            )
        })?;
    if session_group::group_has_active_runs(&group) {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!(
                    "Group '{}' has queued or running session runs. Stop them before deleting the group.",
                    group_id
                )
            })),
        ));
    }
    session_group::delete_group_from_disk_locked(&group_id)
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to delete group: {error}") })),
            )
        })?;
    crate::close_group_client(&state, &group_id).await;
    crate::broadcast_group_list_payload(&state).await;
    Ok(Json(json!({"ok": true, "group_id": group_id})))
}

pub(crate) async fn api_promote_session_group_admin(
    Query(query): Query<GroupMemberQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    crate::validate_local_request_headers(&headers)?;
    let group_id = query.group.as_deref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing group id" })),
        )
    })?;
    let session_id = query.session.as_deref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing session id" })),
        )
    })?;
    let (_, group) = promote_group_admin_from_control(&state, group_id, session_id)
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
    Ok(Json(
        json!({"ok": true, "group": group_json(&state, &group).await}),
    ))
}

pub(crate) async fn api_delete_session_group_member(
    Query(query): Query<GroupMemberQuery>,
    headers: HeaderMap,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    crate::validate_local_request_headers(&headers)?;
    let group_id = query.group.as_deref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing group id" })),
        )
    })?;
    let session_id = query.session.as_deref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "Missing session id" })),
        )
    })?;
    let (_, group) = remove_group_member_direct(&state, group_id, session_id)
        .await
        .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;
    Ok(Json(
        json!({"ok": true, "group": group_json(&state, &group).await}),
    ))
}

#[cfg(test)]
#[path = "tests/session_control_tests.rs"]
mod tests;
