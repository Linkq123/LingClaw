use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    AppState, ChatMessage, MAIN_SESSION_ID, now_epoch,
    runtime_loop::{self, AgentRunMode},
    session_group::{self, GroupMessage, GroupRun, SessionGroup},
    session_store::{self, SessionSummary},
};

static NEXT_CONTROL_ID: AtomicU64 = AtomicU64::new(1);
static DIRECT_RUNS: std::sync::LazyLock<std::sync::Mutex<HashMap<String, DirectRunEntry>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
static GROUP_RUN_CONTROLS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<String, GroupRunControlEntry>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

const DIRECT_RUN_RETAIN_SECS: u64 = 10 * 60;

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
    message: String,
    run_mode: AgentRunMode,
    wait: bool,
    summary_budget: usize,
}

#[derive(Clone, Debug)]
struct StartedRun {
    run_id: String,
    group_id: Option<String>,
    session_id: String,
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
        .filter(|target| !member_set.contains(*target))
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

fn prune_direct_runs_locked(runs: &mut HashMap<String, DirectRunEntry>) {
    let cutoff = now_epoch().saturating_sub(DIRECT_RUN_RETAIN_SECS);
    runs.retain(|_, run| run.status.is_active() || run.updated_at >= cutoff);
}

fn with_direct_runs<R>(f: impl FnOnce(&mut HashMap<String, DirectRunEntry>) -> R) -> R {
    let mut guard = DIRECT_RUNS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let mut guard = GROUP_RUN_CONTROLS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f(&mut guard)
}

fn register_group_run_control(
    run_id: &str,
    group_id: &str,
    _session_id: &str,
    control: &DelegatedRunControl,
) {
    with_group_run_controls(|runs| {
        runs.insert(
            run_id.to_string(),
            GroupRunControlEntry {
                group_id: group_id.to_string(),
                cancel: control.cancel.clone(),
                stop_requested: Arc::clone(&control.stop_requested),
            },
        );
    });
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

fn update_direct_run_status(run_id: &str, status: DirectRunStatus) {
    with_direct_runs(|runs| {
        if let Some(run) = runs.get_mut(run_id) {
            run.status = status;
            run.updated_at = now_epoch();
        }
    });
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

fn stop_direct_runs_for_targets(targets: &[String]) -> usize {
    let target_set = targets.iter().collect::<HashSet<_>>();
    with_direct_runs(|runs| {
        let mut stopped = 0usize;
        for run in runs.values_mut() {
            if !run.status.is_active() || !target_set.contains(&run.session_id) {
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

fn mentions_from_text(text: &str, members: &[String]) -> Vec<String> {
    let member_set = members.iter().collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        let Some(raw) = token.strip_prefix('@') else {
            continue;
        };
        let candidate = raw
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && !matches!(ch, '-' | '_' | '.'));
        if candidate.is_empty() {
            continue;
        }
        let candidate = candidate.to_string();
        if member_set.contains(&candidate) && seen.insert(candidate.clone()) {
            out.push(candidate);
        }
    }
    out
}

async fn mutate_group<F, R>(group_id: &str, f: F) -> Result<(SessionGroup, R), String>
where
    F: FnOnce(&mut SessionGroup) -> R,
{
    let group_id = session_group::validate_group_id(group_id)?.to_string();
    let gate = session_group::group_persist_gate(&group_id);
    let _guard = gate.lock().await;
    let mut group = session_group::load_group_from_disk(&group_id)
        .ok_or_else(|| format!("Group '{}' not found", group_id))?;
    let result = f(&mut group);
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

async fn update_run_status(
    state: &AppState,
    group_id: &str,
    run_id: &str,
    status: &str,
    result_excerpt: Option<String>,
    error: Option<String>,
) -> Option<GroupRun> {
    let updated = mutate_group(group_id, |group| {
        let now = now_epoch();
        if let Some(run) = group.runs.iter_mut().find(|run| run.id == run_id) {
            return apply_group_run_status_transition(
                run,
                status,
                result_excerpt.clone(),
                error.clone(),
                now,
            );
        }
        None
    })
    .await
    .ok()
    .and_then(|(_, run)| run);

    if let Some(run) = updated.as_ref() {
        crate::send_group_client_event(state, group_id, run_status_event(group_id, &run)).await;
        if matches!(status, "completed" | "failed" | "stopped") {
            crate::send_group_client_event(
                state,
                group_id,
                json!({
                    "type": "group_run_completed",
                    "group_id": group_id,
                    "run_id": run.id,
                    "session_id": run.session_id,
                    "status": run.status,
                    "result_excerpt": run.result_excerpt,
                    "error": run.error,
                    "completed_at": run.completed_at,
                }),
            )
            .await;
        }
    }
    updated
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
        session.pending_plan = None;
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
) -> String {
    match group_id {
        Some(group_id) => format!(
            "[Session group: {group_id}]\n\
             Main session asked this session to contribute one response to the group conversation.\n\
             Respond in your own session using your normal tools and permissions. Do not assume other sessions can see private tool outputs unless you summarize them.\n\n\
             Group context summary:\n{}\n\n\
             Main instruction:\n{source_message}",
            group_context.unwrap_or("No group context is available.")
        ),
        None => format!(
            "[Main session delegation]\n\
             Main session asked this session to complete this delegated task. Respond in your own session using your normal tools and permissions.\n\n\
             Main instruction:\n{source_message}"
        ),
    }
}

fn target_group_context(group_id: &str, budget: usize) -> String {
    let mut context = session_group::load_group_from_disk(group_id)
        .map(|group| collect_group_summary(&group))
        .unwrap_or_else(|| "No group context is available.".to_string());
    crate::truncate_safe(&mut context, budget.clamp(500, 8_000));
    context
}

async fn latest_assistant_excerpt_after(
    state: &AppState,
    session_id: &str,
    user_message_index: usize,
    budget: usize,
) -> Option<String> {
    let sessions = state.sessions.lock().await;
    let session = sessions.get(session_id)?;
    latest_assistant_content_after(&session.messages, user_message_index, budget)
}

fn latest_assistant_content_after(
    messages: &[ChatMessage],
    user_message_index: usize,
    budget: usize,
) -> Option<String> {
    let mut content = messages
        .iter()
        .skip(user_message_index.saturating_add(1))
        .rev()
        .find(|message| message.role == "assistant")
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
) {
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
    .ok()
    .map(|(_, message)| message);
    if let Some(message) = message {
        crate::send_group_client_event(
            state,
            group_id,
            json!({"type":"group_message","group_id": group_id,"message": message}),
        )
        .await;
    }
}

async fn group_run_is_active(group_id: &str, run_id: &str) -> bool {
    session_group::load_group_from_disk(group_id)
        .and_then(|group| group.runs.into_iter().find(|run| run.id == run_id))
        .map(|run| matches!(run.status.as_str(), "queued" | "running"))
        .unwrap_or(false)
}

async fn forward_group_live_event(
    state: &AppState,
    group_id: &str,
    run_id: &str,
    session_id: &str,
    event: Value,
) {
    crate::send_group_client_event(
        state,
        group_id,
        json!({
            "type": "group_member_event",
            "group_id": group_id,
            "run_id": run_id,
            "session_id": session_id,
            "event": event,
        }),
    )
    .await;
}

fn spawn_target_run(
    state: Arc<AppState>,
    run: StartedRun,
    prompt: String,
    run_mode: AgentRunMode,
    summary_budget: usize,
) {
    tokio::spawn(async move {
        let run_cancel = run.control.cancel.clone();
        let stop_requested = Arc::clone(&run.control.stop_requested);
        let lock = session_control_lock(&state, &run.session_id).await;
        let _guard = lock.lock().await;
        if let Some(group_id) = run.group_id.as_deref()
            && !group_run_is_active(group_id, &run.run_id).await
        {
            clear_group_run_control(&run.run_id);
            return;
        }
        if run.group_id.is_none() && !direct_run_is_active(&run.run_id) {
            return;
        }
        wait_until_session_idle(&state, &run.session_id, &run_cancel).await;
        if state.shutdown.is_cancelled() || run_cancel.is_cancelled() {
            if run.group_id.is_some() {
                clear_group_run_control(&run.run_id);
            } else {
                update_direct_run_status(&run.run_id, DirectRunStatus::Stopped);
            }
            return;
        }
        if let Some(group_id) = run.group_id.as_deref()
            && !group_run_is_active(group_id, &run.run_id).await
        {
            clear_group_run_control(&run.run_id);
            return;
        }
        if run.group_id.is_none() && !direct_run_is_active(&run.run_id) {
            return;
        }
        if let Some(group_id) = run.group_id.as_deref() {
            if update_run_status(&state, group_id, &run.run_id, "running", None, None)
                .await
                .is_none()
            {
                clear_group_run_control(&run.run_id);
                return;
            }
        } else {
            update_direct_run_status(&run.run_id, DirectRunStatus::Running);
        }

        let connection_id = state.next_connection_id.fetch_add(1, Ordering::Relaxed);
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
                    let _ = update_run_status(
                        &state,
                        group_id,
                        &run.run_id,
                        "failed",
                        None,
                        Some("Target session already has an active run.".to_string()),
                    )
                    .await;
                    clear_group_run_control(&run.run_id);
                } else {
                    update_direct_run_status(&run.run_id, DirectRunStatus::Failed);
                }
                return;
            }
        };

        let user_message_index =
            match append_target_session_message(&state, &run.session_id, prompt).await {
                Ok(index) => index,
                Err(error) => {
                    runtime_loop::release_agent_run_reservation(
                        &state,
                        &run.session_id,
                        &reservation,
                    )
                    .await;
                    if let Some(group_id) = run.group_id.as_deref() {
                        let _ = update_run_status(
                            state.as_ref(),
                            group_id,
                            &run.run_id,
                            "failed",
                            None,
                            Some(error),
                        )
                        .await;
                        clear_group_run_control(&run.run_id);
                    } else {
                        update_direct_run_status(&run.run_id, DirectRunStatus::Failed);
                    }
                    return;
                }
            };

        let (live_tx, mut live_rx) = mpsc::channel::<Value>(crate::LIVE_EVENT_CHANNEL_CAPACITY);
        let dispatch_state = Arc::clone(&state);
        let dispatch_session_id = run.session_id.clone();
        let dispatch_group_id = run.group_id.clone();
        let dispatch_run_id = run.run_id.clone();
        let dispatcher = tokio::spawn(async move {
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
        )
        .await;
        drop(live_tx);
        let _ = dispatcher.await;

        let result_excerpt = latest_assistant_excerpt_after(
            &state,
            &run.session_id,
            user_message_index,
            summary_budget,
        )
        .await
        .unwrap_or_else(|| {
            if outcome.shutting_down {
                "Server shutdown before session produced a final response.".to_string()
            } else {
                "Session run finished without assistant text.".to_string()
            }
        });
        if let Some(group_id) = run.group_id.as_deref() {
            let status = if outcome.shutting_down {
                "stopped"
            } else if outcome.run_stopped {
                "stopped"
            } else {
                "completed"
            };
            if update_run_status(
                &state,
                group_id,
                &run.run_id,
                status,
                Some(result_excerpt.clone()),
                None,
            )
            .await
            .is_some()
            {
                record_group_session_result(
                    &state,
                    group_id,
                    &run.run_id,
                    &run.session_id,
                    result_excerpt,
                )
                .await;
            }
            clear_group_run_control(&run.run_id);
        } else {
            let status = if outcome.shutting_down || outcome.run_stopped {
                DirectRunStatus::Stopped
            } else {
                DirectRunStatus::Completed
            };
            update_direct_run_status(&run.run_id, status);
        }
    });
}

async fn dispatch_to_sessions(
    state: &Arc<AppState>,
    request: DispatchRequest,
) -> Result<Vec<StartedRun>, String> {
    let targets = normalize_target_ids(request.targets);
    let group_members = if let Some(group_id) = request.group_id.as_deref() {
        let group = session_group::load_group_from_disk(group_id)
            .ok_or_else(|| format!("Group '{}' not found", group_id))?;
        Some(group.members)
    } else {
        None
    };
    if targets.is_empty() {
        return Err("No target sessions were selected.".to_string());
    }
    if let Some(members) = group_members.as_ref() {
        validate_group_targets(
            request.group_id.as_deref().unwrap_or_default(),
            members,
            &targets,
        )?;
    }

    let mut canonical_targets = Vec::new();
    let mut seen_canonical_targets = HashSet::new();
    for target in targets {
        let (resolved_session_id, _) =
            runtime_loop::ensure_session_ready(state, Some(&target)).await?;
        if seen_canonical_targets.insert(resolved_session_id.clone()) {
            canonical_targets.push(resolved_session_id);
        }
    }

    let group_context = request
        .group_id
        .as_deref()
        .map(|group_id| target_group_context(group_id, request.summary_budget));
    let prompt = target_prompt(
        request.group_id.as_deref(),
        &request.message,
        group_context.as_deref(),
    );
    let mut runs = Vec::new();
    if let Some(group_id) = request.group_id.as_deref() {
        let group_runs = mutate_group(group_id, |group| {
            let now = now_epoch();
            let mut out = Vec::new();
            for target in &canonical_targets {
                let run = GroupRun {
                    id: next_id("grun"),
                    group_id: group_id.to_string(),
                    session_id: target.clone(),
                    status: "queued".to_string(),
                    prompt: request.message.clone(),
                    result_excerpt: None,
                    error: None,
                    created_at: now,
                    updated_at: now,
                    completed_at: None,
                };
                group.runs.push(run.clone());
                out.push(run);
            }
            out
        })
        .await?
        .1;
        for group_run in group_runs {
            let control = DelegatedRunControl {
                cancel: state.shutdown.child_token(),
                stop_requested: Arc::new(AtomicBool::new(false)),
            };
            register_group_run_control(&group_run.id, group_id, &group_run.session_id, &control);
            crate::send_group_client_event(
                state,
                group_id,
                json!({
                    "type": "group_run_started",
                    "group_id": group_id,
                    "run": group_run,
                }),
            )
            .await;
            runs.push(StartedRun {
                run_id: group_run.id,
                group_id: Some(group_id.to_string()),
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
                session_id,
                control,
            }
        }));
    }

    for run in runs.clone() {
        spawn_target_run(
            Arc::clone(state),
            run,
            prompt.clone(),
            request.run_mode,
            request.summary_budget,
        );
    }

    if request.wait {
        wait_for_runs(state, &runs, request.summary_budget).await;
    }
    Ok(runs)
}

async fn wait_for_runs(state: &AppState, runs: &[StartedRun], _summary_budget: usize) {
    let timeout = state.config().sub_agent_timeout;
    let deadline = run_wait_deadline(timeout);
    loop {
        let mut complete = true;
        for run in runs {
            let active = if let Some(group_id) = run.group_id.as_deref() {
                let status = session_group::load_group_from_disk(group_id)
                    .and_then(|group| group.runs.into_iter().find(|item| item.id == run.run_id))
                    .map(|run| run.status)
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
    let group = session_group::load_group_from_disk(group_id)
        .ok_or_else(|| format!("Group '{}' not found", group_id))?;
    let mut targets = match payload.target_mode.as_str() {
        "selected" => normalize_target_ids(payload.targets),
        "mentions" => mentions_from_text(&text, &group.members),
        "all" | "" => group.members.clone(),
        other => {
            return Err(format!(
                "Invalid target_mode '{}'. Use selected, all, or mentions.",
                other
            ));
        }
    };
    if payload.target_mode == "mentions" && targets.is_empty() {
        targets = group.members.clone();
    }
    let run_mode = parse_run_mode(&payload.run_mode)?;
    let turn_id = next_id("turn");
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
        json!({"type":"group_message","group_id": group_id,"message": message}),
    )
    .await;
    crate::send_group_client_event(
        state,
        group_id,
        session_group::group_history_payload(
            &session_group::load_group_from_disk(group_id)
                .ok_or_else(|| format!("Group '{}' not found", group_id))?,
        ),
    )
    .await;

    if payload.start_runs {
        dispatch_to_sessions(
            state,
            DispatchRequest {
                group_id: Some(group_id.to_string()),
                targets,
                message: text,
                run_mode,
                wait: false,
                summary_budget: 4_000,
            },
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn handle_group_socket_stop(
    state: &AppState,
    group_id: &str,
    targets: Vec<String>,
) -> Result<String, String> {
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
            lines.push(format!(
                "- {} session={} status={} result={}",
                run.id,
                run.session_id,
                run.status,
                run.result_excerpt.as_deref().unwrap_or("")
            ));
        }
    }
    if !group.messages.is_empty() {
        lines.push("Recent messages:".to_string());
        for message in group.messages.iter().rev().take(10).rev() {
            let who = message.session_id.as_deref().unwrap_or(&message.role);
            lines.push(format!("- {}: {}", who, message.content));
        }
    }
    lines.join("\n")
}

fn session_list_output(state: &AppState) -> String {
    let config = state.config();
    let mut summaries =
        session_store::list_saved_session_summaries_in_dir(&session_store::sessions_dir());
    if let Ok(sessions) = state.sessions.try_lock() {
        for session in sessions.values() {
            if !summaries.iter().any(|summary| summary.id == session.id) {
                summaries.push(SessionSummary::from_session(session));
            }
        }
    }
    session_store::sort_session_summaries(&mut summaries);
    let mut lines = vec!["Sessions:".to_string()];
    for summary in summaries {
        let model = session_group::validate_group_id(&summary.id)
            .ok()
            .and_then(|_| session_store::load_session_from_disk(&summary.id))
            .map(|session| session.effective_model(&config.model).to_string())
            .unwrap_or_else(|| config.model.clone());
        lines.push(format!(
            "- {} ({}) messages={} tool_calls={} model={}{}",
            summary.id,
            summary.name,
            summary.messages,
            summary.tool_calls,
            model,
            if summary.corrupt { " corrupt" } else { "" }
        ));
    }
    lines.join("\n")
}

fn group_list_output() -> String {
    let mut lines = vec!["Session groups:".to_string()];
    for summary in session_group::list_saved_group_summaries() {
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
    lines.join("\n")
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

fn tool_args_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

async fn create_group_from_tool(state: &AppState, args: &Value) -> Result<String, String> {
    let group_id = session_group::generate_available_group_id()?;
    let name = tool_args_string(args, "name").unwrap_or_else(|| format!("Group {group_id}"));
    let name = session_group::validate_group_name(&name)?;
    let group = SessionGroup::new(&group_id, &name, tool_args_array(args, "members"));
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
    let group_id = tool_args_string(args, "group_id")
        .ok_or_else(|| "session_control error: group_id is required".to_string())?;
    let name = tool_args_string(args, "name")
        .map(|name| session_group::validate_group_name(&name))
        .transpose()?;
    let members = args
        .get("members")
        .and_then(Value::as_array)
        .map(|_| tool_args_array(args, "members"));
    let group = mutate_group(&group_id, |group| {
        if let Some(name) = name {
            group.name = name;
        }
        if let Some(members) = members {
            group.members = session_group::normalize_members(members);
        }
        group.clone()
    })
    .await?
    .1;
    crate::send_group_client_event(state, &group.id, session_group::group_info_payload(&group))
        .await;
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
    let message = tool_args_string(args, "message")
        .ok_or_else(|| "session_control error: message is required".to_string())?;
    let msg = mutate_group(&group_id, |group| {
        append_group_message(group, "main", None, message.clone(), None, None)
    })
    .await?
    .1;
    crate::send_group_client_event(
        state,
        &group_id,
        json!({"type":"group_message","group_id": group_id,"message": msg}),
    )
    .await;
    crate::broadcast_group_list_payload(state).await;
    Ok(format!("Posted group message {} to {}.", msg.id, group_id))
}

async fn dispatch_from_tool(state: &Arc<AppState>, args: &Value) -> Result<String, String> {
    let group_id = tool_args_string(args, "group_id");
    let mut targets = tool_args_array(args, "targets");
    if targets.is_empty()
        && let Some(group_id) = group_id.as_deref()
        && let Some(group) = session_group::load_group_from_disk(group_id)
    {
        targets = group.members;
    }
    targets = normalize_target_ids(targets);
    if targets.iter().any(|target| target == MAIN_SESSION_ID) {
        return Err(
            "session_control error: cannot dispatch to the main session from session_control."
                .to_string(),
        );
    }
    let message = tool_args_string(args, "message")
        .ok_or_else(|| "session_control error: message is required".to_string())?;
    let run_mode = parse_run_mode(
        args.get("run_mode")
            .and_then(Value::as_str)
            .unwrap_or("execute"),
    )?;
    let wait = args.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let summary_budget = args
        .get("summary_budget")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(4_000)
        .clamp(500, 20_000);
    if let Some(group_id) = group_id.as_deref() {
        let msg = mutate_group(group_id, |group| {
            append_group_message(group, "main", None, message.clone(), None, None)
        })
        .await?
        .1;
        crate::send_group_client_event(
            state,
            group_id,
            json!({"type":"group_message","group_id": group_id,"message": msg}),
        )
        .await;
    }
    let runs = dispatch_to_sessions(
        state,
        DispatchRequest {
            group_id,
            targets,
            message,
            run_mode,
            wait,
            summary_budget,
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
    let group = session_group::load_group_from_disk(group_id)
        .ok_or_else(|| format!("Group '{}' not found", group_id))?;
    if targets.is_empty() {
        targets = group
            .runs
            .into_iter()
            .filter(|run| matches!(run.status.as_str(), "queued" | "running"))
            .map(|run| run.session_id)
            .collect();
    }
    targets = normalize_target_ids(targets);
    if targets.is_empty() {
        return Err("session_control error: no running target sessions selected".to_string());
    }
    let mut stopped_group_runs = 0usize;
    let target_set = targets.iter().cloned().collect::<HashSet<_>>();
    let run_ids = session_group::load_group_from_disk(group_id)
        .ok_or_else(|| format!("Group '{}' not found", group_id))?
        .runs
        .into_iter()
        .filter(|run| {
            target_set.contains(&run.session_id)
                && matches!(run.status.as_str(), "queued" | "running")
        })
        .map(|run| run.id)
        .collect::<Vec<_>>();
    for run_id in &run_ids {
        if update_run_status(state, group_id, run_id, "stopped", None, None)
            .await
            .is_some()
        {
            stopped_group_runs += 1;
        }
    }
    stop_group_run_controls(group_id, &run_ids);
    Ok(format!(
        "Stop requested for {stopped_group_runs} group run(s) and 0 direct run(s)."
    ))
}

async fn stop_from_tool(state: &AppState, args: &Value) -> Result<String, String> {
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
    if current_session_id != MAIN_SESSION_ID {
        return crate::tools::ToolOutcome {
            output: "session_control error: this tool is only available in the main session."
                .to_string(),
            is_error: true,
            duration_ms: 0,
            subagent_snapshot: None,
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
            };
        }
    };
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match action {
        "list_sessions" => Ok(session_list_output(state)),
        "list_groups" => Ok(group_list_output()),
        "create_group" => create_group_from_tool(state, &args).await,
        "update_group" => update_group_from_tool(state, &args).await,
        "post_group_message" => post_group_message_from_tool(state, &args).await,
        "dispatch" => dispatch_from_tool(state, &args).await,
        "collect" => {
            let group_id = tool_args_string(&args, "group_id")
                .ok_or_else(|| "session_control error: group_id is required".to_string());
            match group_id {
                Ok(group_id) => session_group::load_group_from_disk(&group_id)
                    .map(|group| collect_group_summary(&group))
                    .ok_or_else(|| format!("Group '{}' not found", group_id)),
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_CONTROL_REGISTRY_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

    fn control_registry_test_guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_CONTROL_REGISTRY_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn clear_direct_runs_for_test() {
        DIRECT_RUNS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    fn clear_group_run_controls_for_test() {
        GROUP_RUN_CONTROLS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    fn test_direct_control() -> DelegatedRunControl {
        DelegatedRunControl {
            cancel: CancellationToken::new(),
            stop_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    fn test_message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(content.to_string()),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        }
    }

    fn test_group_run(status: &str) -> GroupRun {
        GroupRun {
            id: "grun-test".to_string(),
            group_id: "group-test".to_string(),
            session_id: "worker-a".to_string(),
            status: status.to_string(),
            prompt: "inspect".to_string(),
            result_excerpt: None,
            error: None,
            created_at: 1,
            updated_at: 1,
            completed_at: None,
        }
    }

    #[test]
    fn group_target_prompt_includes_context_and_instruction() {
        let prompt = target_prompt(
            Some("review-group"),
            "Check backend risk",
            Some("Recent messages:\n- main: split the review"),
        );

        assert!(prompt.contains("[Session group: review-group]"));
        assert!(prompt.contains("Group context summary:"));
        assert!(prompt.contains("Recent messages:"));
        assert!(prompt.contains("Main instruction:\nCheck backend risk"));
    }

    #[test]
    fn direct_run_registry_tracks_terminal_status_for_waits() {
        let _guard = control_registry_test_guard();
        clear_direct_runs_for_test();
        let control = test_direct_control();
        register_direct_run("run-test-wait", "worker-a", &control);

        assert!(direct_run_is_active("run-test-wait"));

        update_direct_run_status("run-test-wait", DirectRunStatus::Running);
        assert!(direct_run_is_active("run-test-wait"));

        update_direct_run_status("run-test-wait", DirectRunStatus::Completed);
        assert!(direct_run_status("run-test-wait").is_terminal());
    }

    #[test]
    fn group_run_status_transition_does_not_overwrite_terminal_state() {
        let mut completed = test_group_run("completed");
        completed.result_excerpt = Some("done".to_string());
        completed.completed_at = Some(2);

        assert!(
            apply_group_run_status_transition(&mut completed, "stopped", None, None, 3).is_none()
        );
        assert_eq!(completed.status, "completed");
        assert_eq!(completed.result_excerpt.as_deref(), Some("done"));
        assert_eq!(completed.completed_at, Some(2));

        let mut stopped = test_group_run("queued");
        assert!(
            apply_group_run_status_transition(&mut stopped, "stopped", None, None, 4).is_some()
        );
        assert_eq!(stopped.status, "stopped");
        assert!(
            apply_group_run_status_transition(&mut stopped, "running", None, None, 5).is_none()
        );
        assert_eq!(stopped.status, "stopped");
    }

    #[test]
    fn stop_direct_runs_cancels_queued_matching_targets() {
        let _guard = control_registry_test_guard();
        clear_direct_runs_for_test();
        let control = test_direct_control();
        register_direct_run("run-test-stop", "worker-stop-target", &control);

        let stopped = stop_direct_runs_for_targets(&["worker-stop-target".to_string()]);

        assert_eq!(stopped, 1);
        assert_eq!(direct_run_status("run-test-stop"), DirectRunStatus::Stopped);
        assert!(control.cancel.is_cancelled());
        assert!(control.stop_requested.load(Ordering::Relaxed));
        assert!(!direct_run_is_active("run-test-stop"));
    }

    #[test]
    fn zero_timeout_has_no_wait_deadline() {
        assert!(run_wait_deadline(Duration::ZERO).is_none());
        assert!(run_wait_deadline(Duration::from_millis(1)).is_some());
    }

    #[test]
    fn validate_group_targets_rejects_non_members() {
        let members = vec!["worker-a".to_string(), "worker-b".to_string()];

        let error = validate_group_targets(
            "review-group",
            &members,
            &["worker-b".to_string(), "worker-c".to_string()],
        )
        .expect_err("non-member target should be rejected");

        assert!(error.contains("review-group"));
        assert!(error.contains("worker-c"));
    }

    #[test]
    fn latest_assistant_content_after_ignores_previous_assistant() {
        let messages = vec![
            test_message("user", "old question"),
            test_message("assistant", "old answer"),
            test_message("user", "delegated task"),
        ];

        assert!(latest_assistant_content_after(&messages, 2, 1_000).is_none());

        let mut messages_with_reply = messages;
        messages_with_reply.push(test_message("assistant", "new answer"));

        assert_eq!(
            latest_assistant_content_after(&messages_with_reply, 2, 1_000).as_deref(),
            Some("new answer")
        );
    }

    #[test]
    fn stop_group_run_controls_cancels_only_matching_group_runs() {
        let _guard = control_registry_test_guard();
        clear_group_run_controls_for_test();
        let matching = test_direct_control();
        let other_group = test_direct_control();
        register_group_run_control("run-group-stop", "group-a", "worker-a", &matching);
        register_group_run_control("run-other-group", "group-b", "worker-a", &other_group);

        let stopped = stop_group_run_controls("group-a", &["run-group-stop".to_string()]);

        assert_eq!(stopped, 1);
        assert!(matching.cancel.is_cancelled());
        assert!(matching.stop_requested.load(Ordering::Relaxed));
        assert!(!other_group.cancel.is_cancelled());
        assert!(!other_group.stop_requested.load(Ordering::Relaxed));
    }
}
