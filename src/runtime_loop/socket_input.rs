use super::*;

use std::collections::{BTreeMap, HashSet};

use crate::prompts::build_system_prompt;
use crate::socket_sync::broadcast_session_list_payload;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

/// Structured user message payload from frontend (when images are attached).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputImageAttachment {
    url: String,
    #[serde(default)]
    object_key: Option<String>,
    #[serde(default)]
    attachment_token: Option<String>,
    #[serde(default)]
    s3_config_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserMessagePayload {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    images: Vec<InputImageAttachment>,
    #[serde(default)]
    plan_mode: Option<bool>,
    #[serde(default)]
    execute_plan_id: Option<String>,
    #[serde(default)]
    plan_action: Option<PlanActionPayload>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PlanActionKind {
    Feedback,
    Execute,
    Refresh,
    Discard,
    Resume,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PlanActionPayload {
    pub(super) action: PlanActionKind,
    pub(super) plan_id: String,
    pub(super) revision: u32,
    #[serde(default)]
    pub(super) text: Option<String>,
    #[serde(default)]
    pub(super) answers: BTreeMap<String, String>,
    #[serde(default)]
    pub(super) allow_stale: bool,
    #[serde(default)]
    pub(super) stale_confirmation_token: Option<String>,
}

const PLAN_EVIDENCE_VERIFICATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

fn stale_confirmation_token(plan_id: &str, revision: u32, snapshot_fingerprint: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"lingclaw-plan-stale-confirmation-v1");
    digest.update((plan_id.len() as u64).to_le_bytes());
    digest.update(plan_id.as_bytes());
    digest.update(revision.to_le_bytes());
    digest.update(snapshot_fingerprint.as_bytes());
    format!("{:x}", digest.finalize())
}

fn looks_like_structured_user_payload(trimmed: &str) -> bool {
    let Some(rest) = trimmed.strip_prefix('{') else {
        return false;
    };
    let rest = rest.trim_start();
    [
        "\"text\"",
        "\"images\"",
        "\"plan_mode\"",
        "\"execute_plan_id\"",
        "\"plan_action\"",
    ]
    .iter()
    .any(|field| rest.starts_with(field))
}

pub(super) fn resolve_input_image_url(
    url: &str,
    object_key: Option<&str>,
    attachment_token: Option<&str>,
    s3_config_id: Option<&str>,
    s3_cfg: Option<&crate::config::S3Config>,
) -> Result<(String, Option<String>), String> {
    match (object_key, attachment_token) {
        (Some(object_key), Some(token)) => {
            let cfg = s3_cfg.ok_or_else(|| {
                "S3 uploads are no longer configured. Please re-attach the image.".to_string()
            })?;
            let supplied_config_id = s3_config_id.ok_or_else(|| {
                "Incomplete uploaded image metadata. Please re-attach the image.".to_string()
            })?;
            if supplied_config_id != crate::image_uploads::s3_config_id(cfg) {
                return Err(
                    "S3 upload configuration changed. Please re-attach the image.".to_string(),
                );
            }
            if !crate::image_uploads::verify_attachment_object_key(cfg, object_key, token) {
                return Err("Invalid uploaded image token. Please re-attach the image.".to_string());
            }

            let trusted_url =
                crate::image_uploads::resolve_image_url("", Some(object_key), Some(cfg))?;
            Ok((trusted_url, Some(object_key.to_string())))
        }
        (Some(_), None) | (None, Some(_)) => {
            Err("Incomplete uploaded image metadata. Please re-attach the image.".to_string())
        }
        (None, None) if s3_config_id.is_some() => {
            Err("Incomplete uploaded image metadata. Please re-attach the image.".to_string())
        }
        (None, None) => Ok((url.to_string(), None)),
    }
}

pub(crate) enum IdleSocketInputAction {
    Continue,
    StartAgent {
        run_mode: AgentRunMode,
        reservation: AgentRunReservation,
        model_snapshot: crate::SessionModelSnapshot,
    },
    SwitchSession {
        session_id: String,
        result: crate::commands::CommandResult,
    },
    Break,
}

pub(crate) async fn ensure_session_ready(
    state: &AppState,
    requested_id: Option<&str>,
) -> Result<(String, bool), String> {
    let session_id = match requested_id {
        Some(id) => crate::session_store::validate_session_id(id)?.to_string(),
        None => MAIN_SESSION_ID.to_string(),
    };
    // Serialize creation and canonical lookup using the same gate as persistence.
    // On Windows the gate key is case-insensitive, so concurrent aliases cannot
    // both observe a missing Session and create separate durable rows.
    let persist_gate = crate::session_store::session_persist_gate(&session_id);
    let _persist_guard = persist_gate.lock().await;
    let saved_session_id = crate::session_store::canonical_saved_session_id_result(&session_id)?;
    let effective_session_id = saved_session_id
        .as_deref()
        .unwrap_or(&session_id)
        .to_string();

    let existing_session = {
        let sessions = state.sessions.lock().await;
        sessions
            .keys()
            .find(|existing_id| crate::session_ids_match(existing_id, &effective_session_id))
            .cloned()
            .and_then(|existing_session_id| {
                sessions
                    .get(&existing_session_id)
                    .cloned()
                    .map(|session| (existing_session_id, session))
            })
    };
    if let Some((existing_session_id, session_snapshot)) = existing_session {
        let sys =
            crate::session_store::build_refreshed_session_system_prompt(state, &session_snapshot)
                .await;
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(&existing_session_id) {
            crate::session_store::replace_session_system_prompt(session, sys);
        }
        return Ok((existing_session_id, false));
    }

    let config = state.config();
    let display_name = if crate::is_main(&effective_session_id) {
        "Main".to_string()
    } else {
        effective_session_id.clone()
    };
    #[cfg(not(test))]
    let persisted_session_exists = saved_session_id.is_some();
    #[cfg(test)]
    let persisted_session_exists = {
        let persisted_session_path =
            crate::session_store::sessions_dir().join(format!("{effective_session_id}.json"));
        let persisted_session_tmp_path =
            crate::session_store::sessions_dir().join(format!("{effective_session_id}.json.tmp"));
        saved_session_id.is_some()
            || match tokio::fs::try_exists(&persisted_session_path).await {
                Ok(exists) => exists,
                Err(err) => {
                    return Err(format!(
                        "Failed to inspect persisted session '{}': {err}",
                        session_id
                    ));
                }
            }
            || match tokio::fs::try_exists(&persisted_session_tmp_path).await {
                Ok(exists) => exists,
                Err(err) => {
                    return Err(format!(
                        "Failed to inspect persisted session '{}': {err}",
                        session_id
                    ));
                }
            }
    };
    let mut cleanup_fresh_workspace = false;
    let (mut session, created_fresh) =
        match crate::session_store::load_session_from_storage_result(&effective_session_id)? {
            Some(session) => (session, false),
            None if persisted_session_exists => {
                return Err(format!(
                    "Session '{}' is corrupt and could not be loaded.",
                    effective_session_id
                ));
            }
            None => {
                if !state.storage_is_writable() {
                    return Err(
                        "Local storage is in protected mode. Repair it and restart LingClaw."
                            .to_string(),
                    );
                }
                cleanup_fresh_workspace =
                    !crate::session_workspace_path(&effective_session_id).exists();
                let mut session = Session::new_with_id(&effective_session_id, &display_name);
                let model = session.effective_model(&config.model).to_string();
                let sys = build_system_prompt(
                    &config,
                    &session.workspace,
                    &model,
                    &session.enabled_system_skills,
                );
                session.messages.push(sys);
                (session, true)
            }
        };
    let sys = crate::session_store::build_refreshed_session_system_prompt(state, &session).await;
    crate::session_store::replace_session_system_prompt(&mut session, sys);

    if created_fresh
        && let Err(error) = crate::session_store::save_session_to_disk_locked(&session).await
    {
        if cleanup_fresh_workspace {
            crate::session_control::cleanup_failed_created_session(
                &effective_session_id,
                &session.workspace,
            );
        }
        return Err(format!(
            "Failed to persist newly created Session '{}': {error}",
            effective_session_id
        ));
    }

    let final_session_id = session.id.clone();
    let mut sessions = state.sessions.lock().await;
    sessions.entry(final_session_id.clone()).or_insert(session);
    Ok((final_session_id, created_fresh))
}

pub(crate) async fn resolve_or_create_socket_session(
    state: &AppState,
    tx: &WsTx,
    requested_id: Option<&str>,
    connection_id: u64,
    connection_cancel: &CancellationToken,
) -> String {
    match ensure_session_ready(state, requested_id).await {
        Ok((session_id, created_fresh)) => {
            replace_connection_cancel_binding(state, &session_id, connection_id, connection_cancel)
                .await;
            bind_session_connection(state, &session_id, connection_id, tx, false).await;
            send_existing_session_payloads(tx, state, &session_id).await;
            replay_live_round(tx, state, &session_id).await;
            finish_session_replay(state, &session_id, connection_id).await;
            if created_fresh {
                broadcast_session_list_payload(state).await;
            }
            session_id
        }
        Err(error) => {
            let fallback_session_id = ensure_session_ready(state, None)
                .await
                .map(|(session_id, _)| session_id)
                .unwrap_or_else(|_| MAIN_SESSION_ID.to_string());
            replace_connection_cancel_binding(
                state,
                &fallback_session_id,
                connection_id,
                connection_cancel,
            )
            .await;
            bind_session_connection(state, &fallback_session_id, connection_id, tx, false).await;
            send_existing_session_payloads(tx, state, &fallback_session_id).await;
            replay_live_round(tx, state, &fallback_session_id).await;
            finish_session_replay(state, &fallback_session_id, connection_id).await;
            ws_send(
                tx,
                &json!({
                    "type":"error",
                    "content": error,
                    "dismissible": true,
                }),
            )
            .await;
            fallback_session_id
        }
    }
}

pub(crate) async fn known_session_ids(state: &AppState) -> Result<HashSet<String>, String> {
    let mut known_ids =
        crate::session_store::list_saved_session_ids_result(&crate::session_store::sessions_dir())
            .map_err(|_| {
                "Local storage is in protected mode. Repair it and restart LingClaw.".to_string()
            })?;
    let sessions = state.sessions.lock().await;
    known_ids.extend(sessions.keys().cloned());
    known_ids.insert(MAIN_SESSION_ID.to_string());
    Ok(known_ids)
}

pub(crate) async fn resolve_session_target_for_command(
    state: &AppState,
    target: &str,
) -> Result<String, String> {
    let known_ids = known_session_ids(state).await?;
    crate::session_store::resolve_session_target(target, &known_ids)
}

pub(crate) async fn resolve_session_target_for_delete(
    state: &AppState,
    target: &str,
) -> Result<String, String> {
    resolve_session_target_for_command(state, target).await
}

fn plan_error(code: &str, content: impl Into<String>) -> serde_json::Value {
    json!({
        "type": "error",
        "code": code,
        "content": content.into(),
        "dismissible": true,
    })
}

async fn reject_message_for_active_plan(
    state: &Arc<AppState>,
    session_id: &str,
    tx: &WsTx,
) -> bool {
    let active_plan = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(session_id)
            .and_then(|session| session.pending_plan.as_ref())
            .filter(|plan| plan.status.is_active())
            .map(|plan| (plan.id.clone(), plan.revision, plan.status.label()))
    };
    let Some((plan_id, revision, status)) = active_plan else {
        return false;
    };
    ws_send(
        tx,
        &json!({
            "type": "error",
            "code": "plan_already_active",
            "content": "Execute, revise, or discard the active plan before sending another message.",
            "dismissible": true,
            "plan_id": plan_id,
            "revision": revision,
            "status": status,
        }),
    )
    .await;
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlanActionMutationError {
    StaleRevision,
    InvalidStatus(crate::plan::PlanStatus),
}

fn plan_action_status_allowed(action: PlanActionKind, plan: &crate::PendingPlan) -> bool {
    match action {
        PlanActionKind::Feedback => plan.status.can_receive_feedback(),
        PlanActionKind::Execute => plan.status == crate::plan::PlanStatus::Ready,
        PlanActionKind::Refresh | PlanActionKind::Discard => matches!(
            plan.status,
            crate::plan::PlanStatus::Planning
                | crate::plan::PlanStatus::NeedsInput
                | crate::plan::PlanStatus::Ready
                | crate::plan::PlanStatus::Failed
                | crate::plan::PlanStatus::Stopped
        ),
        PlanActionKind::Resume => {
            matches!(
                plan.status,
                crate::plan::PlanStatus::Failed | crate::plan::PlanStatus::Stopped
            ) && plan.approved_at.is_some()
                && plan.execution_attempt > 0
        }
    }
}

fn plan_action_mutation_error(
    action: PlanActionKind,
    error: PlanActionMutationError,
    current_plan: Option<&crate::PendingPlan>,
) -> serde_json::Value {
    let mut event = match error {
        PlanActionMutationError::StaleRevision => {
            plan_error("stale_plan_revision", "The plan changed.")
        }
        PlanActionMutationError::InvalidStatus(crate::plan::PlanStatus::Executing)
            if matches!(action, PlanActionKind::Discard) =>
        {
            plan_error(
                "plan_already_active",
                "Stop the executing plan before discarding it.",
            )
        }
        PlanActionMutationError::InvalidStatus(_) => {
            plan_error("plan_not_ready", "The plan is not ready for this action.")
        }
    };
    if matches!(error, PlanActionMutationError::StaleRevision)
        && let Some(plan) = current_plan
    {
        event["plan"] = plan.to_live_value();
    }
    event
}

async fn plan_action_model_snapshot(
    state: &Arc<AppState>,
    session_id: &str,
    tx: &WsTx,
) -> Option<crate::SessionModelSnapshot> {
    let snapshot = crate::session_model_snapshot(state, session_id).await;
    let Some(snapshot) = snapshot else {
        ws_send(
            tx,
            &plan_error("session_not_found", "Current session not found."),
        )
        .await;
        return None;
    };
    if !snapshot.explicit {
        ws_send(
            tx,
            &plan_error(
                "agent_model_unconfigured",
                "Configure an explicit model before starting an Agent run.",
            ),
        )
        .await;
        return None;
    }
    Some(snapshot)
}

fn build_plan_feedback_text(
    plan: &crate::PendingPlan,
    text: Option<&str>,
    answers: &BTreeMap<String, String>,
) -> Result<String, serde_json::Value> {
    let mut sections = Vec::new();
    if let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) {
        if text.chars().count() > 16_000 {
            return Err(plan_error(
                "invalid_plan_feedback",
                "Plan feedback is too long.",
            ));
        }
        sections.push(text.to_string());
    }
    if !answers.is_empty() {
        let questions = plan
            .artifact
            .questions
            .iter()
            .map(|question| (question.id.as_str(), question))
            .collect::<BTreeMap<_, _>>();
        let mut lines = vec!["Answers to the blocking plan questions:".to_string()];
        for (question_id, answer) in answers {
            let Some(question) = questions.get(question_id.as_str()) else {
                return Err(plan_error(
                    "invalid_plan_feedback",
                    format!("Unknown plan question '{question_id}'."),
                ));
            };
            let answer = answer.trim();
            if answer.is_empty() || answer.chars().count() > 4_000 {
                return Err(plan_error(
                    "invalid_plan_feedback",
                    format!("Answer for question '{question_id}' is empty or too long."),
                ));
            }
            lines.push(format!("- {}: {}", question.prompt, answer));
        }
        sections.push(lines.join("\n"));
    }
    if sections.is_empty() {
        return Err(plan_error(
            "invalid_plan_feedback",
            "Provide feedback or answer at least one plan question.",
        ));
    }
    Ok(sections.join("\n\n"))
}

async fn reject_unavailable_run_workspace(
    state: &AppState,
    current_session_id: &str,
    tx: &WsTx,
) -> bool {
    match super::agent_run_workspace_available(state, current_session_id).await {
        Some(true) | None => return false,
        Some(false) => {}
    }
    ws_send(
        tx,
        &json!({
            "type":"error",
            "code":"workspace_unavailable",
            "content":"The Session working directory is unavailable. Rebind it before starting an Agent run.",
            "dismissible":true,
        }),
    )
    .await;
    true
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_plan_action(
    action: PlanActionPayload,
    current_session_id: &str,
    connection_id: u64,
    state: &Arc<AppState>,
    tx: &WsTx,
    cancel: &CancellationToken,
    stop_requested: &Arc<AtomicBool>,
) -> IdleSocketInputAction {
    let snapshot = {
        let sessions = state.sessions.lock().await;
        sessions.get(current_session_id).and_then(|session| {
            session
                .pending_plan
                .as_ref()
                .map(|plan| (plan.clone(), session.working_directory.clone()))
        })
    };
    let Some((plan_snapshot, workspace)) = snapshot else {
        ws_send(
            tx,
            &plan_error("plan_not_ready", "No plan is available for this Session."),
        )
        .await;
        return IdleSocketInputAction::Continue;
    };
    if plan_snapshot.id != action.plan_id || plan_snapshot.revision != action.revision {
        let mut event = plan_error(
            "stale_plan_revision",
            format!(
                "This plan changed. Reload revision {} before continuing.",
                plan_snapshot.revision
            ),
        );
        event["plan"] = plan_snapshot.to_live_value();
        ws_send(tx, &event).await;
        return IdleSocketInputAction::Continue;
    }

    if matches!(action.action, PlanActionKind::Discard) {
        if plan_snapshot.status == crate::plan::PlanStatus::Executing {
            ws_send(
                tx,
                &plan_error(
                    "plan_already_active",
                    "Stop the executing plan before discarding it.",
                ),
            )
            .await;
            return IdleSocketInputAction::Continue;
        }
        if !plan_action_status_allowed(action.action, &plan_snapshot) {
            ws_send(
                tx,
                &plan_error(
                    "plan_not_ready",
                    "This plan cannot be discarded in its current state.",
                ),
            )
            .await;
            return IdleSocketInputAction::Continue;
        }
        let Some(reservation) = super::try_reserve_agent_run(
            state,
            current_session_id,
            connection_id,
            cancel,
            stop_requested,
        )
        .await
        else {
            ws_send(
                tx,
                &plan_error("plan_already_active", "Session already has an active run."),
            )
            .await;
            return IdleSocketInputAction::Continue;
        };
        let persist_gate = crate::session_store::session_persist_gate(current_session_id);
        let _persist_guard = persist_gate.lock().await;
        let mutation = {
            let mut sessions = state.sessions.lock().await;
            (|| {
                let session = sessions
                    .get_mut(current_session_id)
                    .ok_or(PlanActionMutationError::StaleRevision)?;
                let previous = session.clone();
                let discarded = {
                    let plan = session
                        .pending_plan
                        .as_mut()
                        .ok_or(PlanActionMutationError::StaleRevision)?;
                    if plan.id != action.plan_id || plan.revision != action.revision {
                        return Err(PlanActionMutationError::StaleRevision);
                    }
                    if !plan_action_status_allowed(action.action, plan) {
                        return Err(PlanActionMutationError::InvalidStatus(plan.status));
                    }
                    let now = now_epoch();
                    plan.status = crate::plan::PlanStatus::Discarded;
                    plan.updated_at = now;
                    plan.finished_at = Some(now);
                    plan.pending_feedback = None;
                    plan.clone()
                };
                let now = discarded.updated_at;
                session.updated_at = now;
                Ok((previous, session.clone(), discarded))
            })()
        };
        let (previous, session_to_save, discarded) = match mutation {
            Ok(mutation) => mutation,
            Err(error) => {
                super::release_agent_run_reservation(state, current_session_id, &reservation).await;
                let current_plan = {
                    let sessions = state.sessions.lock().await;
                    sessions
                        .get(current_session_id)
                        .and_then(|session| session.pending_plan.clone())
                };
                ws_send(
                    tx,
                    &plan_action_mutation_error(action.action, error, current_plan.as_ref()),
                )
                .await;
                return IdleSocketInputAction::Continue;
            }
        };
        if let Err(error) =
            crate::session_store::save_session_to_disk_locked(&session_to_save).await
        {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                *session = previous;
            }
            drop(sessions);
            super::release_agent_run_reservation(state, current_session_id, &reservation).await;
            eprintln!("ERROR: could not discard plan: {error}");
            crate::send_storage_status(tx, state).await;
            if state.storage_is_writable() {
                ws_send(
                    tx,
                    &plan_error(
                        "storage_error",
                        "The plan could not be discarded because it was not saved.",
                    ),
                )
                .await;
            }
            return IdleSocketInputAction::Continue;
        }
        super::release_agent_run_reservation(state, current_session_id, &reservation).await;
        ws_send(
            tx,
            &json!({"type":"plan_state", "plan": discarded.to_live_value()}),
        )
        .await;
        return IdleSocketInputAction::Continue;
    }

    let (run_mode, clear_evidence, plan_action_prompt, pending_feedback) = match action.action {
        PlanActionKind::Feedback => {
            if !plan_snapshot.status.can_receive_feedback() {
                ws_send(
                    tx,
                    &plan_error(
                        "plan_not_ready",
                        "This plan cannot be revised in its current state.",
                    ),
                )
                .await;
                return IdleSocketInputAction::Continue;
            }
            let feedback = match build_plan_feedback_text(
                &plan_snapshot,
                action.text.as_deref(),
                &action.answers,
            ) {
                Ok(feedback) => feedback,
                Err(event) => {
                    ws_send(tx, &event).await;
                    return IdleSocketInputAction::Continue;
                }
            };
            (
                AgentRunMode::PlanOnly,
                false,
                Some(feedback.clone()),
                Some(feedback),
            )
        }
        PlanActionKind::Refresh => {
            if !plan_action_status_allowed(action.action, &plan_snapshot) {
                ws_send(
                    tx,
                    &plan_error(
                        "plan_not_ready",
                        "This plan cannot be refreshed in its current state.",
                    ),
                )
                .await;
                return IdleSocketInputAction::Continue;
            }
            (
                AgentRunMode::PlanOnly,
                true,
                Some("Refresh this plan against the current workspace state and submit a new revision.".to_string()),
                None,
            )
        }
        PlanActionKind::Execute | PlanActionKind::Resume => {
            let allowed = plan_action_status_allowed(action.action, &plan_snapshot);
            if !allowed {
                ws_send(
                    tx,
                    &plan_error("plan_not_ready", "The plan is not ready for this action."),
                )
                .await;
                return IdleSocketInputAction::Continue;
            }
            (AgentRunMode::Execute, false, None, None)
        }
        PlanActionKind::Discard => unreachable!(),
    };

    let Some(model_snapshot) = plan_action_model_snapshot(state, current_session_id, tx).await
    else {
        return IdleSocketInputAction::Continue;
    };
    if reject_unavailable_run_workspace(state, current_session_id, tx).await {
        return IdleSocketInputAction::Continue;
    }
    let Some(mut reservation) = super::try_reserve_agent_run(
        state,
        current_session_id,
        connection_id,
        cancel,
        stop_requested,
    )
    .await
    else {
        ws_send(
            tx,
            &plan_error("plan_already_active", "Session already has an active run."),
        )
        .await;
        return IdleSocketInputAction::Continue;
    };
    reservation.reset_plan_evidence = clear_evidence;
    reservation.plan_action_prompt = plan_action_prompt;

    let evidence_snapshot = if run_mode == AgentRunMode::Execute {
        let evidence = plan_snapshot.evidence.clone();
        let workspace_for_check = workspace.clone();
        let verification_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_stop = std::sync::Arc::clone(&verification_stop);
        let verification_cancel = reservation.run_cancel.clone();
        let deadline = std::time::Instant::now() + PLAN_EVIDENCE_VERIFICATION_TIMEOUT;
        let verification = tokio::task::spawn_blocking(move || {
            crate::plan::verify_evidence_snapshot_until(
                &workspace_for_check,
                &evidence,
                deadline,
                worker_stop.as_ref(),
            )
        });
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                verification_stop.store(true, Ordering::Relaxed);
                super::release_agent_run_reservation(state, current_session_id, &reservation).await;
                return IdleSocketInputAction::Break;
            }
            _ = verification_cancel.cancelled() => {
                verification_stop.store(true, Ordering::Relaxed);
                super::release_agent_run_reservation(state, current_session_id, &reservation).await;
                return IdleSocketInputAction::Continue;
            }
            result = tokio::time::timeout(
                PLAN_EVIDENCE_VERIFICATION_TIMEOUT + std::time::Duration::from_millis(250),
                verification,
            ) => result,
        };
        match result {
            Ok(Ok(Ok(paths))) => paths,
            Ok(Ok(Err(crate::plan::EvidenceVerificationError::Cancelled))) => {
                super::release_agent_run_reservation(state, current_session_id, &reservation).await;
                return IdleSocketInputAction::Break;
            }
            Ok(Ok(Err(crate::plan::EvidenceVerificationError::TimedOut))) | Err(_) => {
                verification_stop.store(true, Ordering::Relaxed);
                super::release_agent_run_reservation(state, current_session_id, &reservation).await;
                ws_send(
                    tx,
                    &plan_error(
                        "plan_evidence_verification_failed",
                        "Plan evidence verification timed out. Refresh the plan and try again.",
                    ),
                )
                .await;
                return IdleSocketInputAction::Continue;
            }
            Ok(Err(error)) => {
                eprintln!("ERROR: plan evidence verification worker failed: {error}");
                super::release_agent_run_reservation(state, current_session_id, &reservation).await;
                ws_send(
                    tx,
                    &plan_error(
                        "plan_evidence_verification_failed",
                        "Plan evidence could not be verified. Refresh the plan and try again.",
                    ),
                )
                .await;
                return IdleSocketInputAction::Continue;
            }
        }
    } else {
        crate::plan::EvidenceVerificationSnapshot {
            stale_paths: Vec::new(),
            fingerprint: String::new(),
        }
    };
    let crate::plan::EvidenceVerificationSnapshot {
        stale_paths,
        fingerprint,
    } = evidence_snapshot;
    let evidence_incomplete = run_mode == AgentRunMode::Execute && plan_snapshot.evidence_truncated;
    let requires_confirmation = evidence_incomplete || !stale_paths.is_empty();
    let confirmation_token = requires_confirmation
        .then(|| stale_confirmation_token(&plan_snapshot.id, plan_snapshot.revision, &fingerprint));
    let stale_override_confirmed = confirmation_token.as_deref().is_some_and(|expected| {
        action.allow_stale && action.stale_confirmation_token.as_deref() == Some(expected)
    });
    if requires_confirmation && !stale_override_confirmed {
        super::release_agent_run_reservation(state, current_session_id, &reservation).await;
        ws_send(
            tx,
            &json!({
                "type": "plan_stale",
                "code": "plan_stale",
                "plan_id": plan_snapshot.id,
                "revision": plan_snapshot.revision,
                "paths": stale_paths,
                "evidence_incomplete": evidence_incomplete,
                "confirmation_token": confirmation_token,
            }),
        )
        .await;
        return IdleSocketInputAction::Continue;
    }

    let persist_gate = crate::session_store::session_persist_gate(current_session_id);
    let _persist_guard = persist_gate.lock().await;
    let mutation = {
        let mut sessions = state.sessions.lock().await;
        (|| {
            let session = sessions
                .get_mut(current_session_id)
                .ok_or(PlanActionMutationError::StaleRevision)?;
            let previous = session.clone();
            let active_plan = {
                let plan = session
                    .pending_plan
                    .as_mut()
                    .ok_or(PlanActionMutationError::StaleRevision)?;
                if plan.id != action.plan_id || plan.revision != action.revision {
                    return Err(PlanActionMutationError::StaleRevision);
                }
                if !plan_action_status_allowed(action.action, plan) {
                    return Err(PlanActionMutationError::InvalidStatus(plan.status));
                }
                let now = now_epoch();
                match run_mode {
                    AgentRunMode::PlanOnly => {
                        plan.status = crate::plan::PlanStatus::Planning;
                        plan.updated_at = now;
                        plan.approved_at = None;
                        plan.finished_at = None;
                        plan.pending_feedback = pending_feedback.clone();
                    }
                    AgentRunMode::Execute => {
                        plan.status = crate::plan::PlanStatus::Executing;
                        plan.updated_at = now;
                        plan.approved_at.get_or_insert(now);
                        plan.finished_at = None;
                        plan.execution_attempt = plan.execution_attempt.saturating_add(1);
                        if stale_override_confirmed {
                            plan.stale_override_paths = stale_paths.clone();
                            plan.stale_override_confirmed_at = Some(now);
                        }
                        plan.pending_feedback = None;
                    }
                }
                plan.clone()
            };
            session.updated_at = active_plan.updated_at;
            Ok((previous, session.clone(), active_plan))
        })()
    };
    let (previous, session_to_save, active_plan) = match mutation {
        Ok(mutation) => mutation,
        Err(error) => {
            super::release_agent_run_reservation(state, current_session_id, &reservation).await;
            let current_plan = {
                let sessions = state.sessions.lock().await;
                sessions
                    .get(current_session_id)
                    .and_then(|session| session.pending_plan.clone())
            };
            ws_send(
                tx,
                &plan_action_mutation_error(action.action, error, current_plan.as_ref()),
            )
            .await;
            return IdleSocketInputAction::Continue;
        }
    };
    if let Err(error) = crate::session_store::save_session_to_disk_locked(&session_to_save).await {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(current_session_id) {
            *session = previous;
        }
        super::release_agent_run_reservation(state, current_session_id, &reservation).await;
        eprintln!("ERROR: could not save plan action: {error}");
        crate::send_storage_status(tx, state).await;
        if state.storage_is_writable() {
            ws_send(
                tx,
                &plan_error(
                    "storage_error",
                    "The approved plan could not be saved, so the Agent run was not started.",
                ),
            )
            .await;
        }
        return IdleSocketInputAction::Continue;
    }
    ws_send(
        tx,
        &json!({"type":"plan_state", "plan": active_plan.to_live_value()}),
    )
    .await;
    IdleSocketInputAction::StartAgent {
        run_mode,
        reservation,
        model_snapshot,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_idle_socket_input(
    text: String,
    current_session_id: &mut String,
    _current_session_ref: &Arc<Mutex<String>>,
    connection_id: u64,
    state: &Arc<AppState>,
    tx: &WsTx,
    _live_tx: &LiveTx,
    cancel: &CancellationToken,
    stop_requested: &Arc<AtomicBool>,
) -> IdleSocketInputAction {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return IdleSocketInputAction::Continue;
    }

    let active_run = {
        let runs = state.active_runs.lock().await;
        runs.get(current_session_id).cloned()
    };
    if let Some(run) = active_run {
        if trimmed.eq_ignore_ascii_case("/stop") {
            run.stop_requested.store(true, Ordering::Relaxed);
            run.cancel.cancel();
            ws_send(
                tx,
                &json!({
                    "type":"system",
                    "content":"Stop requested.",
                    "dismissible": true,
                }),
            )
            .await;
            return IdleSocketInputAction::Continue;
        }

        if !state.storage_is_writable() {
            crate::send_storage_status(tx, state).await;
            return IdleSocketInputAction::Continue;
        }

        if let Some(events) = build_busy_command_events(trimmed, current_session_id, state).await {
            for event in events {
                let _ = ws_send(tx, &event).await;
            }
            return IdleSocketInputAction::Continue;
        }

        if let Some((intervention_text, had_images)) = extract_busy_intervention(trimmed) {
            if enqueue_shared_intervention(&run.deferred_interventions, intervention_text).await {
                ws_send(
                    tx,
                    &json!({
                        "type":"progress",
                        "content": busy_intervention_notice(had_images),
                    }),
                )
                .await;
            } else {
                ws_send(
                    tx,
                    &json!({
                        "type":"system",
                        "content":"The active run is already finishing. Please resend after it completes.",
                        "dismissible": true,
                    }),
                )
                .await;
            }
            return IdleSocketInputAction::Continue;
        }

        if trimmed.starts_with('/') {
            ws_send(
                tx,
                &json!({
                    "type":"system",
                    "content":"A run is already in progress. Use /stop or wait for it to finish.",
                    "dismissible": true,
                }),
            )
            .await;
            return IdleSocketInputAction::Continue;
        }

        ws_send(
            tx,
            &json!({
                "type":"system",
                "content":"A run is already in progress. Wait for it to finish or use /stop.",
                "dismissible": true,
            }),
        )
        .await;
        return IdleSocketInputAction::Continue;
    }

    if !state.storage_is_writable() {
        let command = trimmed
            .strip_prefix('/')
            .and_then(|value| value.split_whitespace().next())
            .unwrap_or_default();
        let read_only_command = matches!(
            command.to_ascii_lowercase().as_str(),
            "help" | "status" | "usage" | "sessions"
        );
        if !read_only_command {
            ws_send(
                tx,
                &json!({
                    "type": "storage_status",
                    "storage": {
                        "mode": "protected",
                        "code": "storage_protected",
                    },
                }),
            )
            .await;
            return IdleSocketInputAction::Continue;
        }
    }

    if trimmed.starts_with('/') {
        let command = trimmed.split_whitespace().next().unwrap_or_default();
        if command.eq_ignore_ascii_case("/new")
            && !crate::session_has_explicit_model(state, current_session_id).await
        {
            ws_send(
                tx,
                &json!({
                    "type":"error",
                    "content":"Configure an explicit model before using /new.",
                    "dismissible":true,
                }),
            )
            .await;
            return IdleSocketInputAction::Continue;
        }
        let mut cmd_result = handle_command(
            trimmed,
            current_session_id,
            connection_id,
            state,
            tx,
            cancel,
        )
        .await;
        let mut model_configuration_broadcasted = false;
        if let Some(payloads) = cmd_result
            .as_mut()
            .and_then(|result| result.model_configuration_payloads.take())
        {
            // A successful `/model` has already committed its global revision.
            // Deliver the all-client batch before cancellation checks, hooks,
            // or any origin-only socket output can delay synchronization.
            crate::socket_sync::send_model_configuration_payloads(state, payloads).await;
            model_configuration_broadcasted = true;
        }
        if cancel.is_cancelled() {
            return IdleSocketInputAction::Break;
        }

        // ── OnCommand hook (post-execution, observational) ───────────────
        let (cmd_name, cmd_args) = trimmed
            .split_once(char::is_whitespace)
            .unwrap_or((trimmed, ""));
        let result_type = cmd_result
            .as_ref()
            .map(|r| r.response_type.to_string())
            .unwrap_or_else(|| "unknown_command".to_string());
        let hook_input = CommandHookInput {
            command: cmd_name.to_string(),
            args: cmd_args.to_string(),
            result_type,
            session_id: current_session_id.clone(),
        };
        let config = state.config();
        let hook_events = run_command_hooks(&state.hooks, &hook_input, &config).await;
        for ev in hook_events {
            ws_send(tx, &ev).await;
        }

        if let Some(result) = cmd_result {
            if let Some(session_id) = result.switch_to_session.clone() {
                return IdleSocketInputAction::SwitchSession { session_id, result };
            }
            send_command_refresh(tx, state, current_session_id, result.refresh_history).await;

            ws_send(
                tx,
                &json!({
                    "type": result.response_type,
                    "content": result.response,
                    "dismissible": result.dismissible,
                }),
            )
            .await;

            if result.sessions_changed && !model_configuration_broadcasted {
                let payload = {
                    let model_status_guard = crate::CONFIG_FILE_LOCK.read().await;
                    let (config, config_revision) = state.config_snapshot_with_revision();
                    let payload = {
                        let sessions = state.sessions.lock().await;
                        let (
                            name,
                            model,
                            effort,
                            model_override_present,
                            model_override_configured,
                            effective_model_configured,
                        ) = sessions
                            .get(current_session_id.as_str())
                            .map(|s| {
                                let (model, model_override_configured, effective_model_configured) =
                                    s.model_configuration(&config);
                                let effort = config.normalize_model_effort(&model, &s.think_level);
                                (
                                    s.name.clone(),
                                    model,
                                    effort,
                                    s.model_override.is_some(),
                                    model_override_configured,
                                    effective_model_configured,
                                )
                            })
                            .unwrap_or_else(|| {
                                (
                                    "Main".to_string(),
                                    config.model.clone(),
                                    config.normalize_model_effort(&config.model, "auto"),
                                    false,
                                    false,
                                    config.explicit_primary_model_configured,
                                )
                            });
                        let usage = sessions
                            .get(current_session_id.as_str())
                            .map(crate::socket_sync::build_session_usage_payload)
                            .unwrap_or_else(|| json!({}));
                        build_session_info_payload(
                            current_session_id,
                            &name,
                            &config,
                            &model,
                            &effort,
                            model_override_present,
                            model_override_configured,
                            effective_model_configured,
                            config_revision,
                            usage,
                        )
                    };
                    drop(model_status_guard);
                    payload
                };
                ws_send(tx, &payload).await;
            }
            if result.session_list_changed {
                broadcast_session_list_payload(state).await;
            }
        } else {
            ws_send(
                tx,
                &json!({
                    "type":"system",
                    "content":"Unknown command. Type /help.",
                    "dismissible": true,
                }),
            )
            .await;
        }
        return IdleSocketInputAction::Continue;
    }

    // Try parsing as structured JSON message (with image attachments or UI run options).
    let (msg_text, msg_images, run_mode, image_validation_snapshot) = if trimmed.starts_with('{') {
        match serde_json::from_str::<UserMessagePayload>(trimmed) {
            Ok(payload) => {
                if let Some(action) = payload.plan_action {
                    let has_conflicting_fields = payload.text.is_some()
                        || !payload.images.is_empty()
                        || payload.plan_mode.is_some()
                        || payload.execute_plan_id.is_some();
                    if has_conflicting_fields {
                        ws_send(
                            tx,
                            &plan_error(
                                "invalid_plan_action",
                                "plan_action cannot be combined with text, images, plan_mode, or execute_plan_id.",
                            ),
                        )
                        .await;
                        return IdleSocketInputAction::Continue;
                    }
                    return handle_plan_action(
                        action,
                        current_session_id,
                        connection_id,
                        state,
                        tx,
                        cancel,
                        stop_requested,
                    )
                    .await;
                }
                if let Some(plan_id) = payload.execute_plan_id.as_deref() {
                    let has_conflicting_fields = payload.text.is_some()
                        || !payload.images.is_empty()
                        || payload.plan_mode.is_some();
                    if has_conflicting_fields {
                        ws_send(
                            tx,
                            &plan_error(
                                "invalid_plan_action",
                                "execute_plan_id cannot be combined with text, images, or plan_mode.",
                            ),
                        )
                        .await;
                        return IdleSocketInputAction::Continue;
                    }
                    let plan = {
                        let sessions = state.sessions.lock().await;
                        sessions
                            .get(current_session_id)
                            .and_then(|session| session.pending_plan.as_ref())
                            .filter(|plan| plan.id == plan_id)
                            .cloned()
                    };
                    let Some(plan) = plan else {
                        ws_send(
                            tx,
                            &plan_error(
                                "plan_not_ready",
                                "The requested plan is no longer available for this Session.",
                            ),
                        )
                        .await;
                        return IdleSocketInputAction::Continue;
                    };
                    if plan.revision != 1 {
                        let mut event = plan_error(
                            "stale_plan_revision",
                            "This plan has been revised. Reload it before approving execution.",
                        );
                        event["plan"] = plan.to_live_value();
                        ws_send(tx, &event).await;
                        return IdleSocketInputAction::Continue;
                    }
                    return handle_plan_action(
                        PlanActionPayload {
                            action: PlanActionKind::Execute,
                            plan_id: plan_id.to_string(),
                            revision: plan.revision,
                            text: None,
                            answers: BTreeMap::new(),
                            allow_stale: false,
                            stale_confirmation_token: None,
                        },
                        current_session_id,
                        connection_id,
                        state,
                        tx,
                        cancel,
                        stop_requested,
                    )
                    .await;
                }
                let Some(text) = payload.text else {
                    ws_send(
                        tx,
                        &json!({
                            "type":"system",
                            "content":"Structured messages must include text or execute_plan_id.",
                            "dismissible": true,
                        }),
                    )
                    .await;
                    return IdleSocketInputAction::Continue;
                };
                let requested_run_mode = if payload.plan_mode.unwrap_or(false) {
                    AgentRunMode::PlanOnly
                } else {
                    AgentRunMode::Execute
                };
                if reject_message_for_active_plan(state, current_session_id, tx).await {
                    return IdleSocketInputAction::Continue;
                }
                // Limit images per message to prevent abuse.
                const MAX_IMAGES_PER_MESSAGE: usize = 10;
                if payload.images.len() > MAX_IMAGES_PER_MESSAGE {
                    ws_send(tx, &json!({"type":"system","content":format!("Too many images (max {MAX_IMAGES_PER_MESSAGE}).")})).await;
                    return IdleSocketInputAction::Continue;
                }
                // Server-side capability gate: reject images if model doesn't support them.
                if !payload.images.is_empty() {
                    let Some(provisional_model_snapshot) =
                        crate::session_model_snapshot(state, current_session_id).await
                    else {
                        ws_send(
                            tx,
                            &json!({
                                "type":"error",
                                "content":"Current session not found.",
                            }),
                        )
                        .await;
                        return IdleSocketInputAction::Continue;
                    };
                    if !provisional_model_snapshot.explicit {
                        ws_send(
                            tx,
                            &json!({
                                "type":"error",
                                "content":"Configure an explicit model before starting an Agent run.",
                                "dismissible":true,
                            }),
                        )
                        .await;
                        return IdleSocketInputAction::Continue;
                    }
                    let config = provisional_model_snapshot.config.clone();
                    let workspace = {
                        let sessions = state.sessions.lock().await;
                        sessions
                            .get(current_session_id.as_str())
                            .map(|s| s.workspace.clone())
                            .unwrap_or_else(|| crate::session_workspace_path(current_session_id))
                    };
                    let model = provisional_model_snapshot.model.clone();
                    if !config.model_supports_image(&model) {
                        ws_send(tx, &json!({"type":"system","content":"Current model does not support image input."})).await;
                        return IdleSocketInputAction::Continue;
                    }
                    let prefetch_for_ollama =
                        matches!(config.resolve_model(&model).provider, Provider::Ollama);

                    // Validate image URLs before accepting.
                    let mut validated = Vec::new();
                    let safe_http = if prefetch_for_ollama {
                        Some(match crate::providers::build_image_fetch_client() {
                            Ok(c) => c,
                            Err(err) => {
                                ws_send(tx, &json!({"type":"system","content":err})).await;
                                return IdleSocketInputAction::Continue;
                            }
                        })
                    } else {
                        None
                    };
                    for img in payload.images {
                        let (image_url, trusted_object_key) = match resolve_input_image_url(
                            &img.url,
                            img.object_key.as_deref(),
                            img.attachment_token.as_deref(),
                            img.s3_config_id.as_deref(),
                            config.s3.as_ref(),
                        ) {
                            Ok(resolved) => resolved,
                            Err(message) => {
                                ws_send(tx, &json!({"type":"system","content":message})).await;
                                return IdleSocketInputAction::Continue;
                            }
                        };
                        let is_trusted_upload = trusted_object_key.is_some();
                        let trusted_s3_config_id = trusted_object_key
                            .as_ref()
                            .and_then(|_| img.s3_config_id.clone());

                        let validation = if is_trusted_upload {
                            Ok(())
                        } else {
                            crate::tools::net::validate_image_url(&image_url).await
                        };

                        match validation {
                            Ok(()) => {
                                if let Some(http) = safe_http.as_ref() {
                                    // Only Ollama needs local base64 data; persist it to the
                                    // session workspace so historical images survive restarts.
                                    let fetch_result = if is_trusted_upload {
                                        crate::providers::fetch_single_image_base64_trusted(
                                            &image_url, http,
                                        )
                                        .await
                                    } else {
                                        crate::providers::fetch_single_image_base64(
                                            &image_url, http,
                                        )
                                        .await
                                    };
                                    match fetch_result {
                                        Ok(b64) => {
                                            match crate::providers::persist_image_base64_cache(
                                                &workspace, &image_url, &b64,
                                            )
                                            .await
                                            {
                                                Ok(cache_path) => validated.push(ImageAttachment {
                                                    url: image_url,
                                                    name: None,
                                                    mime_type: None,
                                                    s3_object_key: trusted_object_key,
                                                    s3_config_id: trusted_s3_config_id,
                                                    cache_path: Some(cache_path),
                                                    data: Some(b64),
                                                }),
                                                Err(err) => {
                                                    ws_send(
                                                        tx,
                                                        &json!({"type":"system","content":err}),
                                                    )
                                                    .await;
                                                    return IdleSocketInputAction::Continue;
                                                }
                                            }
                                        }
                                        Err(err) => {
                                            ws_send(tx, &json!({"type":"system","content":err}))
                                                .await;
                                            return IdleSocketInputAction::Continue;
                                        }
                                    }
                                } else {
                                    validated.push(ImageAttachment {
                                        url: image_url,
                                        name: None,
                                        mime_type: None,
                                        s3_object_key: trusted_object_key,
                                        s3_config_id: trusted_s3_config_id,
                                        cache_path: None,
                                        data: None,
                                    });
                                }
                            }
                            Err(err) => {
                                ws_send(tx, &json!({"type":"system","content":err})).await;
                                return IdleSocketInputAction::Continue;
                            }
                        }
                    }
                    let images = if validated.is_empty() {
                        None
                    } else {
                        Some(validated)
                    };
                    (text, images, requested_run_mode, Some((config, model)))
                } else {
                    (text, None, requested_run_mode, None)
                }
            }
            Err(err) => {
                if looks_like_structured_user_payload(trimmed) {
                    ws_send(
                        tx,
                        &json!({
                            "type":"system",
                            "content":format!("Invalid structured message JSON: {err}"),
                            "dismissible": true,
                        }),
                    )
                    .await;
                    return IdleSocketInputAction::Continue;
                }
                (text, None, AgentRunMode::Execute, None)
            }
        }
    } else {
        (text, None, AgentRunMode::Execute, None)
    };

    if reject_message_for_active_plan(state, current_session_id, tx).await {
        return IdleSocketInputAction::Continue;
    }

    if reject_unavailable_run_workspace(state, current_session_id, tx).await {
        return IdleSocketInputAction::Continue;
    }

    let Some(reservation) = super::try_reserve_agent_run(
        state,
        current_session_id,
        connection_id,
        cancel,
        stop_requested,
    )
    .await
    else {
        ws_send(
            tx,
            &json!({
                "type":"system",
                "content":"Session already has an active run.",
                "dismissible": true,
            }),
        )
        .await;
        return IdleSocketInputAction::Continue;
    };

    let Some(model_snapshot) = crate::session_model_snapshot(state, current_session_id).await
    else {
        super::release_agent_run_reservation(state, current_session_id, &reservation).await;
        ws_send(
            tx,
            &json!({
                "type":"error",
                "content":"Current session not found.",
            }),
        )
        .await;
        return IdleSocketInputAction::Continue;
    };
    if !model_snapshot.explicit {
        super::release_agent_run_reservation(state, current_session_id, &reservation).await;
        ws_send(
            tx,
            &json!({
                "type":"error",
                "content":"Configure an explicit model before starting an Agent run.",
                "dismissible":true,
            }),
        )
        .await;
        return IdleSocketInputAction::Continue;
    }
    if let Some((validation_config, validation_model)) = image_validation_snapshot.as_ref()
        && (!Arc::ptr_eq(validation_config, &model_snapshot.config)
            || validation_model != &model_snapshot.model)
    {
        super::release_agent_run_reservation(state, current_session_id, &reservation).await;
        ws_send(
            tx,
            &json!({
                "type":"system",
                "content":"Model or attachment configuration changed while validating images. Attach the images again and resend.",
                "dismissible":true,
            }),
        )
        .await;
        return IdleSocketInputAction::Continue;
    }
    if msg_images.as_ref().is_some_and(|images| !images.is_empty())
        && !model_snapshot
            .config
            .model_supports_image(&model_snapshot.model)
    {
        super::release_agent_run_reservation(state, current_session_id, &reservation).await;
        ws_send(
            tx,
            &json!({
                "type":"system",
                "content":"Current model does not support image input.",
            }),
        )
        .await;
        return IdleSocketInputAction::Continue;
    }

    let initial_plan_artifact = if run_mode.is_plan_only() {
        let has_images = msg_images.as_ref().is_some_and(|images| !images.is_empty());
        match crate::plan::initial_placeholder_artifact(&msg_text, has_images) {
            Ok(artifact) => Some(artifact),
            Err(error) => {
                super::release_agent_run_reservation(state, current_session_id, &reservation).await;
                ws_send(tx, &plan_error("invalid_plan_request", error)).await;
                return IdleSocketInputAction::Continue;
            }
        }
    } else {
        None
    };

    let persist_gate = crate::session_store::session_persist_gate(current_session_id);
    let _persist_guard = persist_gate.lock().await;
    let mutation = {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(current_session_id) {
            let previous_session = session.clone();
            let now = now_epoch();
            session.messages.push(ChatMessage {
                role: "user".into(),
                content: Some(msg_text),
                images: msg_images,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(now),
            });
            if let Some(artifact) = initial_plan_artifact.clone() {
                let user_message_index = session.messages.len() - 1;
                let plan_id = format!("plan_{now}_{user_message_index}_pending");
                let mut pending_plan = crate::PendingPlan::new(
                    plan_id,
                    user_message_index,
                    user_message_index,
                    now,
                    1,
                    crate::plan::PlanStatus::Planning,
                    artifact,
                    Vec::new(),
                    false,
                );
                pending_plan.initial_submission_pending = true;
                session.pending_plan = Some(pending_plan);
            } else if !session.pending_plan.as_ref().is_some_and(|plan| {
                matches!(
                    plan.status,
                    crate::plan::PlanStatus::Failed | crate::plan::PlanStatus::Stopped
                )
            }) {
                session.pending_plan = None;
            }
            session.updated_at = now;
            Some((previous_session, session.clone()))
        } else {
            None
        }
    };

    let Some((previous_session, session_to_save)) = mutation else {
        drop(_persist_guard);
        super::release_agent_run_reservation(state, current_session_id, &reservation).await;
        ws_send(
            tx,
            &json!({
                "type":"error",
                "content":"Current session not found.",
            }),
        )
        .await;
        return IdleSocketInputAction::Continue;
    };

    if let Err(error) = crate::session_store::save_session_to_disk_locked(&session_to_save).await {
        {
            let mut sessions = state.sessions.lock().await;
            if let Some(session) = sessions.get_mut(current_session_id) {
                *session = previous_session;
            }
        }
        drop(_persist_guard);
        super::release_agent_run_reservation(state, current_session_id, &reservation).await;
        eprintln!("ERROR: failed to persist session before starting Agent: {error}");
        crate::send_storage_status(tx, state).await;
        if state.storage_is_writable() {
            ws_send(
                tx,
                &json!({
                    "type":"error",
                    "content":"The message could not be saved, so the Agent run was not started.",
                    "dismissible":true,
                }),
            )
            .await;
        }
        return IdleSocketInputAction::Continue;
    }
    let planning_plan = run_mode
        .is_plan_only()
        .then_some(session_to_save.pending_plan.as_ref())
        .flatten()
        .cloned();
    drop(_persist_guard);
    if let Some(plan) = planning_plan {
        ws_send(
            tx,
            &json!({"type":"plan_state", "plan": plan.to_live_value()}),
        )
        .await;
    }

    IdleSocketInputAction::StartAgent {
        run_mode,
        reservation,
        model_snapshot,
    }
}

pub(super) async fn persist_pending_interventions(
    state: &Arc<AppState>,
    current_session_id: &str,
    pending_interventions: &mut Vec<String>,
) -> bool {
    if pending_interventions.is_empty() {
        return false;
    }
    if !state.storage_is_writable() {
        return false;
    }

    let persist_gate = crate::session_store::session_persist_gate(current_session_id);
    let _persist_guard = persist_gate.lock().await;
    if !state.storage_is_writable() {
        return false;
    }

    let drained = std::mem::take(pending_interventions);
    let mutation = {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(current_session_id) {
            let previous = session.clone();
            for text in &drained {
                session.messages.push(ChatMessage {
                    role: "user".into(),
                    content: Some(text.clone()),
                    images: None,
                    thinking: None,
                    anthropic_thinking_blocks: None,
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: Some(now_epoch()),
                });
            }
            session.updated_at = now_epoch();
            Some((previous, session.clone()))
        } else {
            None
        }
    };

    let Some((previous, session_to_save)) = mutation else {
        pending_interventions.extend(drained);
        return false;
    };

    if let Err(error) = crate::session_store::save_session_to_disk_locked(&session_to_save).await {
        let mut sessions = state.sessions.lock().await;
        if sessions.get(current_session_id).is_some_and(|current| {
            serde_json::to_vec(current).ok() == serde_json::to_vec(&session_to_save).ok()
        }) {
            sessions.insert(current_session_id.to_string(), previous);
        }
        pending_interventions.extend(drained);
        eprintln!("ERROR: failed to save pending interventions: {error}");
        return false;
    }

    true
}

pub(super) async fn drain_shared_interventions(
    shared_interventions: &Arc<Mutex<DeferredInterventionState>>,
    pending_interventions: &mut Vec<String>,
) {
    let drained = {
        let mut shared = shared_interventions.lock().await;
        std::mem::take(&mut shared.queue)
    };
    pending_interventions.extend(drained);
}

pub(super) async fn close_shared_interventions(
    shared_interventions: &Arc<Mutex<DeferredInterventionState>>,
    pending_interventions: &mut Vec<String>,
) {
    let drained = {
        let mut shared = shared_interventions.lock().await;
        shared.accepting = false;
        std::mem::take(&mut shared.queue)
    };
    pending_interventions.extend(drained);
}

async fn enqueue_shared_intervention(
    shared_interventions: &Arc<Mutex<DeferredInterventionState>>,
    intervention_text: String,
) -> bool {
    let mut shared = shared_interventions.lock().await;
    if !shared.accepting {
        return false;
    }
    shared.queue.push(intervention_text);
    true
}

fn parse_busy_allowlisted_command(trimmed: &str) -> Option<(&str, &str)> {
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().map(str::trim).unwrap_or("");
    if cmd.eq_ignore_ascii_case("/think") {
        Some(("/think", arg))
    } else {
        None
    }
}

async fn build_busy_command_events(
    trimmed: &str,
    current_session_id: &str,
    state: &Arc<AppState>,
) -> Option<Vec<serde_json::Value>> {
    let (cmd, arg) = parse_busy_allowlisted_command(trimmed)?;
    let mut result = match cmd {
        "/think" => crate::commands::handle_think_command(arg, current_session_id, state).await,
        _ => return None,
    };

    if let Some(payloads) = result.model_configuration_payloads.take() {
        // Busy `/think` is persisted immediately and must publish the same
        // revision to every bound Session/Group client as the idle command
        // path. The origin-only `session` event below remains for compatibility.
        crate::socket_sync::send_model_configuration_payloads(state, payloads).await;
    }

    let hook_input = CommandHookInput {
        command: cmd.to_string(),
        args: arg.to_string(),
        result_type: result.response_type.to_string(),
        session_id: current_session_id.to_string(),
    };
    let hook_config = state.config();
    let mut events = run_command_hooks(&state.hooks, &hook_input, &hook_config).await;

    let response = if result.sessions_changed {
        format!(
            "{}\nWill apply on the next reasoning cycle if this run continues.",
            result.response
        )
    } else {
        result.response
    };
    events.push(json!({
        "type": result.response_type,
        "content": response,
        "dismissible": result.dismissible,
    }));

    if result.sessions_changed {
        let payload = {
            let model_status_guard = crate::CONFIG_FILE_LOCK.read().await;
            let (config, config_revision) = state.config_snapshot_with_revision();
            let payload = {
                let sessions = state.sessions.lock().await;
                let (
                    name,
                    model,
                    effort,
                    model_override_present,
                    model_override_configured,
                    effective_model_configured,
                ) = sessions
                    .get(current_session_id)
                    .map(|s| {
                        let (model, model_override_configured, effective_model_configured) =
                            s.model_configuration(&config);
                        let effort = config.normalize_model_effort(&model, &s.think_level);
                        (
                            s.name.clone(),
                            model,
                            effort,
                            s.model_override.is_some(),
                            model_override_configured,
                            effective_model_configured,
                        )
                    })
                    .unwrap_or_else(|| {
                        (
                            "Main".to_string(),
                            config.model.clone(),
                            config.normalize_model_effort(&config.model, "auto"),
                            false,
                            false,
                            config.explicit_primary_model_configured,
                        )
                    });
                let usage = sessions
                    .get(current_session_id)
                    .map(crate::socket_sync::build_session_usage_payload)
                    .unwrap_or_else(|| json!({}));
                crate::socket_sync::build_session_info_payload(
                    current_session_id,
                    &name,
                    &config,
                    &model,
                    &effort,
                    model_override_present,
                    model_override_configured,
                    effective_model_configured,
                    config_revision,
                    usage,
                )
            };
            drop(model_status_guard);
            payload
        };
        events.push(payload);
    }
    if result.session_list_changed {
        match crate::socket_sync::build_session_list_payload(state).await {
            Ok(payload) => events.push(payload),
            Err(_) => events.push(json!({
                "type": "storage_status",
                "storage": crate::storage_status_payload(state),
            })),
        }
    }

    Some(events)
}

fn extract_busy_intervention(trimmed: &str) -> Option<(String, bool)> {
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return None;
    }

    if trimmed.starts_with('{') {
        match serde_json::from_str::<UserMessagePayload>(trimmed) {
            Ok(payload) => payload.text.map(|text| (text, !payload.images.is_empty())),
            Err(_) => Some((trimmed.to_string(), false)),
        }
    } else {
        Some((trimmed.to_string(), false))
    }
}

fn busy_intervention_notice(had_images: bool) -> &'static str {
    if had_images {
        "📝 Intervention received (text only — image attachments are not supported during active runs). Will apply at next reasoning cycle."
    } else {
        "📝 Intervention received — will apply at next reasoning cycle"
    }
}

/// Maximum messages to drain in a single tick to prevent starvation.
const MAX_DRAIN_PER_TICK: usize = 64;

pub(super) async fn drain_busy_socket_messages(
    state: &Arc<AppState>,
    current_session_id: &str,
    inbound_rx: &mut mpsc::Receiver<String>,
    pending_interventions: &mut Vec<String>,
    live_tx: &LiveTx,
    run_cancel: &CancellationToken,
) -> bool {
    let mut drained = 0;
    let mut storage_status_sent = false;
    while drained < MAX_DRAIN_PER_TICK {
        let msg = match inbound_rx.try_recv() {
            Ok(msg) => msg,
            Err(_) => break,
        };
        drained += 1;
        let trimmed = msg.trim();
        if trimmed.eq_ignore_ascii_case("/stop") {
            run_cancel.cancel();
            return true;
        }
        if !state.storage_is_writable() {
            if !storage_status_sent {
                let _ = live_send(
                    live_tx,
                    json!({
                        "type": "storage_status",
                        "storage": crate::storage_status_payload(state),
                    }),
                )
                .await;
                storage_status_sent = true;
            }
            continue;
        }
        if let Some(events) = build_busy_command_events(trimmed, current_session_id, state).await {
            for event in events {
                let _ = live_send(live_tx, event).await;
            }
            continue;
        }
        if let Some((intervention_text, had_images)) = extract_busy_intervention(trimmed) {
            pending_interventions.push(intervention_text);
            let notice = busy_intervention_notice(had_images);
            let _ = live_send(live_tx, json!({"type":"progress","content":notice})).await;
        }
    }

    false
}
