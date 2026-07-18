use super::*;

use std::collections::HashSet;

use crate::prompts::build_system_prompt;
use crate::session_store::load_session_from_disk;
use crate::socket_sync::broadcast_session_list_payload;
use serde::Deserialize;
use serde_json::json;

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
}

const APPROVED_PLAN_EXECUTION_PREFIX: &str = "Proceed with the approved plan.";

fn approved_plan_execution_message() -> String {
    APPROVED_PLAN_EXECUTION_PREFIX.to_string()
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
    let saved_session_id = crate::session_store::canonical_saved_session_id(&session_id);
    let effective_session_id = saved_session_id
        .as_deref()
        .unwrap_or(&session_id)
        .to_string();

    {
        let mut sessions = state.sessions.lock().await;
        if let Some(existing_session_id) = sessions
            .keys()
            .find(|existing_id| crate::session_ids_match(existing_id, &effective_session_id))
            .cloned()
        {
            let session = sessions
                .get_mut(&existing_session_id)
                .expect("existing session id should still be present");
            refresh_session_system_prompt(state, session);
            return Ok((existing_session_id, false));
        }
    }

    let config = state.config();
    let display_name = if crate::is_main(&effective_session_id) {
        "Main".to_string()
    } else {
        effective_session_id.clone()
    };
    let persisted_session_path =
        crate::session_store::sessions_dir().join(format!("{effective_session_id}.json"));
    let persisted_session_tmp_path =
        crate::session_store::sessions_dir().join(format!("{effective_session_id}.json.tmp"));
    let persisted_session_exists = saved_session_id.is_some()
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
        };
    let (mut session, created_fresh) = match load_session_from_disk(&effective_session_id) {
        Some(session) => (session, false),
        None if persisted_session_exists => {
            return Err(format!(
                "Session '{}' is corrupt and could not be loaded.",
                effective_session_id
            ));
        }
        None => {
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
    refresh_session_system_prompt(state, &mut session);

    if created_fresh && let Err(error) = save_session_to_disk(&session).await {
        eprintln!(
            "Warning: failed to persist session {} on creation: {error}; keeping in memory",
            effective_session_id
        );
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

pub(crate) async fn known_session_ids(state: &AppState) -> HashSet<String> {
    let mut known_ids =
        crate::session_store::list_saved_session_ids_in_dir(&crate::session_store::sessions_dir());
    let sessions = state.sessions.lock().await;
    known_ids.extend(sessions.keys().cloned());
    known_ids.insert(MAIN_SESSION_ID.to_string());
    known_ids
}

pub(crate) async fn resolve_session_target_for_command(
    state: &AppState,
    target: &str,
) -> Result<String, String> {
    let known_ids = known_session_ids(state).await;
    crate::session_store::resolve_session_target(target, &known_ids)
}

pub(crate) async fn resolve_session_target_for_delete(
    state: &AppState,
    target: &str,
) -> Result<String, String> {
    resolve_session_target_for_command(state, target).await
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
                            model_override_present,
                            model_override_configured,
                            effective_model_configured,
                        ) = sessions
                            .get(current_session_id.as_str())
                            .map(|s| {
                                let (model, model_override_configured, effective_model_configured) =
                                    s.model_configuration(&config);
                                (
                                    s.name.clone(),
                                    model,
                                    s.model_override.is_some(),
                                    model_override_configured,
                                    effective_model_configured,
                                )
                            })
                            .unwrap_or_else(|| {
                                (
                                    "Main".to_string(),
                                    config.model.clone(),
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
                if let Some(plan_id) = payload.execute_plan_id.as_deref() {
                    let has_conflicting_fields = payload.text.is_some()
                        || !payload.images.is_empty()
                        || payload.plan_mode.is_some();
                    if has_conflicting_fields {
                        ws_send(
                            tx,
                            &json!({
                                "type":"system",
                                "content":"execute_plan_id cannot be combined with text, images, or plan_mode.",
                                "dismissible": true,
                            }),
                        )
                        .await;
                        return IdleSocketInputAction::Continue;
                    }

                    let execute_plan_error = {
                        let sessions = state.sessions.lock().await;
                        match sessions.get(current_session_id) {
                            None => Some(json!({
                                "type":"error",
                                "content":"Current session not found.",
                            })),
                            Some(session) => match session.pending_plan.as_ref().cloned() {
                                None => Some(json!({
                                    "type":"system",
                                    "content":"No pending plan is available to execute.",
                                    "dismissible": true,
                                })),
                                Some(pending_plan) if pending_plan.id != plan_id => Some(json!({
                                    "type":"system",
                                    "content":"The requested plan is no longer pending for this session.",
                                    "dismissible": true,
                                })),
                                Some(_) => None,
                            },
                        }
                    };
                    if let Some(event) = execute_plan_error {
                        ws_send(tx, &event).await;
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
                    let Some(model_snapshot) =
                        crate::session_model_snapshot(state, current_session_id).await
                    else {
                        super::release_agent_run_reservation(
                            state,
                            current_session_id,
                            &reservation,
                        )
                        .await;
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
                        super::release_agent_run_reservation(
                            state,
                            current_session_id,
                            &reservation,
                        )
                        .await;
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
                    let execute_plan_error = {
                        let mut sessions = state.sessions.lock().await;
                        match sessions.get_mut(current_session_id) {
                            None => Some(json!({
                                "type":"error",
                                "content":"Current session not found.",
                            })),
                            Some(session) => match session.pending_plan.as_ref().cloned() {
                                None => Some(json!({
                                    "type":"system",
                                    "content":"No pending plan is available to execute.",
                                    "dismissible": true,
                                })),
                                Some(pending_plan) if pending_plan.id != plan_id => Some(json!({
                                    "type":"system",
                                    "content":"The requested plan is no longer pending for this session.",
                                    "dismissible": true,
                                })),
                                Some(_) => {
                                    let execution_message = approved_plan_execution_message();
                                    session.pending_plan = None;
                                    session.messages.push(ChatMessage {
                                        role: "user".into(),
                                        content: Some(execution_message),
                                        images: None,
                                        thinking: None,
                                        anthropic_thinking_blocks: None,
                                        tool_calls: None,
                                        tool_call_id: None,
                                        timestamp: Some(now_epoch()),
                                    });
                                    session.updated_at = now_epoch();
                                    None
                                }
                            },
                        }
                    };
                    if let Some(event) = execute_plan_error {
                        super::release_agent_run_reservation(
                            state,
                            current_session_id,
                            &reservation,
                        )
                        .await;
                        ws_send(tx, &event).await;
                        return IdleSocketInputAction::Continue;
                    }
                    if let Err(e) = crate::session_store::save_current_session_to_disk(
                        state,
                        current_session_id,
                    )
                    .await
                    {
                        eprintln!("Warning: failed to save session before executing plan: {e}");
                    }
                    return IdleSocketInputAction::StartAgent {
                        run_mode: AgentRunMode::Execute,
                        reservation,
                        model_snapshot,
                    };
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

    let appended = {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(current_session_id) {
            session.messages.push(ChatMessage {
                role: "user".into(),
                content: Some(msg_text),
                images: msg_images,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(now_epoch()),
            });
            session.pending_plan = None;
            session.updated_at = now_epoch();
            true
        } else {
            false
        }
    };

    if !appended {
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
    }

    if let Err(e) =
        crate::session_store::save_current_session_to_disk(state, current_session_id).await
    {
        eprintln!("Warning: failed to save session before starting agent: {e}");
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

    let drained = std::mem::take(pending_interventions);
    let changed = {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(current_session_id) {
            for text in drained {
                session.messages.push(ChatMessage {
                    role: "user".into(),
                    content: Some(text),
                    images: None,
                    thinking: None,
                    anthropic_thinking_blocks: None,
                    tool_calls: None,
                    tool_call_id: None,
                    timestamp: Some(now_epoch()),
                });
            }
            session.pending_plan = None;
            session.updated_at = now_epoch();
            true
        } else {
            false
        }
    };

    if changed
        && let Err(e) =
            crate::session_store::save_current_session_to_disk(state, current_session_id).await
    {
        eprintln!("Warning: failed to save pending interventions: {e}");
    }
    changed
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
    let result = match cmd {
        "/think" => crate::commands::handle_think_command(arg, current_session_id, state).await,
        _ => return None,
    };

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
                    model_override_present,
                    model_override_configured,
                    effective_model_configured,
                ) = sessions
                    .get(current_session_id)
                    .map(|s| {
                        let (model, model_override_configured, effective_model_configured) =
                            s.model_configuration(&config);
                        (
                            s.name.clone(),
                            model,
                            s.model_override.is_some(),
                            model_override_configured,
                            effective_model_configured,
                        )
                    })
                    .unwrap_or_else(|| {
                        (
                            "Main".to_string(),
                            config.model.clone(),
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
        events.push(crate::socket_sync::build_session_list_payload(state));
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
