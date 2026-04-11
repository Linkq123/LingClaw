use super::*;

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
}

#[derive(Deserialize)]
struct UserMessagePayload {
    text: String,
    #[serde(default)]
    images: Vec<InputImageAttachment>,
}

pub(super) fn resolve_input_image_url(
    url: &str,
    object_key: Option<&str>,
    attachment_token: Option<&str>,
    s3_cfg: Option<&crate::config::S3Config>,
) -> Result<(String, Option<String>), String> {
    match (object_key, attachment_token) {
        (Some(object_key), Some(token)) => {
            let cfg = s3_cfg.ok_or_else(|| {
                "S3 uploads are no longer configured. Please re-attach the image.".to_string()
            })?;
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
        (None, None) => Ok((url.to_string(), None)),
    }
}

pub(crate) enum IdleSocketInputAction {
    Continue,
    StartAgent,
    Break,
}

pub(crate) async fn resolve_or_create_socket_session(
    state: &Arc<AppState>,
    tx: &WsTx,
    _requested_id: Option<&str>,
    _connection_id: u64,
) -> String {
    ensure_main_session_ready(state).await;
    send_existing_session_payloads(tx, state, MAIN_SESSION_ID).await;
    MAIN_SESSION_ID.to_string()
}

async fn ensure_main_session_ready(state: &Arc<AppState>) {
    {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(MAIN_SESSION_ID) {
            refresh_session_system_prompt(state, session);
            return;
        }
    }

    let (mut session, created_fresh) = match load_session_from_disk(MAIN_SESSION_ID) {
        Some(session) => (session, false),
        None => {
            let mut session = Session::new_with_id(MAIN_SESSION_ID, "Main");
            let model = session.effective_model(&state.config.model).to_string();
            let sys = build_system_prompt(
                &state.config,
                &session.workspace,
                &model,
                &session.disabled_system_skills,
            );
            session.messages.push(sys);
            (session, true)
        }
    };
    refresh_session_system_prompt(state, &mut session);

    if created_fresh && let Err(error) = save_session_to_disk(&session).await {
        eprintln!(
            "Warning: failed to persist main session on creation: {error}; keeping in memory"
        );
    }

    let mut sessions = state.sessions.lock().await;
    sessions
        .entry(MAIN_SESSION_ID.to_string())
        .or_insert(session);
}

pub(crate) async fn handle_idle_socket_input(
    text: String,
    current_session_id: &mut String,
    _current_session_ref: &Arc<Mutex<String>>,
    connection_id: u64,
    state: &Arc<AppState>,
    tx: &WsTx,
    cancel: &CancellationToken,
) -> IdleSocketInputAction {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return IdleSocketInputAction::Continue;
    }

    if trimmed.starts_with('/') {
        let cmd_result = handle_command(
            trimmed,
            current_session_id,
            connection_id,
            state,
            tx,
            cancel,
        )
        .await;
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
        let hook_events = run_command_hooks(&state.hooks, &hook_input, &state.config).await;
        for ev in hook_events {
            ws_send(tx, &ev).await;
        }

        if let Some(result) = cmd_result {
            send_command_refresh(tx, state, current_session_id, result.refresh_history).await;

            ws_send(
                tx,
                &json!({"type":result.response_type,"content":result.response}),
            )
            .await;

            if result.sessions_changed {
                let payload = {
                    let sessions = state.sessions.lock().await;
                    let (name, model) = sessions
                        .get(current_session_id.as_str())
                        .map(|s| {
                            (
                                s.name.clone(),
                                s.effective_model(&state.config.model).to_string(),
                            )
                        })
                        .unwrap_or_else(|| ("Main".to_string(), state.config.model.clone()));
                    build_session_info_payload(current_session_id, &name, state, &model)
                };
                ws_send(tx, &payload).await;
            }
        } else {
            ws_send(
                tx,
                &json!({"type":"system","content":"Unknown command. Type /help."}),
            )
            .await;
        }
        return IdleSocketInputAction::Continue;
    }

    // Try parsing as structured JSON message (with image attachments).
    let (msg_text, msg_images) = if trimmed.starts_with('{') {
        match serde_json::from_str::<UserMessagePayload>(trimmed) {
            Ok(payload) => {
                // Limit images per message to prevent abuse.
                const MAX_IMAGES_PER_MESSAGE: usize = 10;
                if payload.images.len() > MAX_IMAGES_PER_MESSAGE {
                    ws_send(tx, &json!({"type":"system","content":format!("Too many images (max {MAX_IMAGES_PER_MESSAGE}).")})).await;
                    return IdleSocketInputAction::Continue;
                }
                // Server-side capability gate: reject images if model doesn't support them.
                if !payload.images.is_empty() {
                    let (model, workspace) = {
                        let sessions = state.sessions.lock().await;
                        sessions
                            .get(current_session_id.as_str())
                            .map(|s| {
                                (
                                    s.effective_model(&state.config.model).to_string(),
                                    s.workspace.clone(),
                                )
                            })
                            .unwrap_or_else(|| {
                                (
                                    state.config.model.clone(),
                                    crate::session_workspace_path(current_session_id),
                                )
                            })
                    };
                    if !state.config.model_supports_image(&model) {
                        ws_send(tx, &json!({"type":"system","content":"Current model does not support image input."})).await;
                        return IdleSocketInputAction::Continue;
                    }
                    let prefetch_for_ollama = matches!(
                        state.config.resolve_model(&model).provider,
                        Provider::Ollama
                    );

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
                            state.config.s3.as_ref(),
                        ) {
                            Ok(resolved) => resolved,
                            Err(message) => {
                                ws_send(tx, &json!({"type":"system","content":message})).await;
                                return IdleSocketInputAction::Continue;
                            }
                        };
                        let is_trusted_upload = trusted_object_key.is_some();

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
                                                    s3_object_key: trusted_object_key,
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
                                        s3_object_key: trusted_object_key,
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
                    (payload.text, images)
                } else {
                    // No images — treat the raw input as plain text so
                    // accidental JSON like {"text":"hello"} is not consumed.
                    (text, None)
                }
            }
            Err(_) => (text, None),
        }
    } else {
        (text, None)
    };

    {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(current_session_id) {
            session.messages.push(ChatMessage {
                role: "user".into(),
                content: Some(msg_text),
                images: msg_images,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(now_epoch()),
            });
            session.updated_at = now_epoch();
        }
    }

    IdleSocketInputAction::StartAgent
}

pub(super) async fn persist_pending_interventions(
    state: &Arc<AppState>,
    current_session_id: &str,
    pending_interventions: &mut Vec<String>,
) {
    if pending_interventions.is_empty() {
        return;
    }

    let drained = std::mem::take(pending_interventions);
    let mut sessions = state.sessions.lock().await;
    if let Some(session) = sessions.get_mut(current_session_id) {
        for text in drained {
            session.messages.push(ChatMessage {
                role: "user".into(),
                content: Some(text),
                images: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(now_epoch()),
            });
        }
        session.updated_at = now_epoch();
    }
}

/// Maximum messages to drain in a single tick to prevent starvation.
const MAX_DRAIN_PER_TICK: usize = 64;

pub(super) async fn drain_busy_socket_messages(
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
        if !trimmed.is_empty() && !trimmed.starts_with('/') {
            // Extract text from structured JSON payloads (images dropped during busy state).
            let (intervention_text, had_images) = if trimmed.starts_with('{') {
                match serde_json::from_str::<UserMessagePayload>(trimmed) {
                    Ok(p) => {
                        let had = !p.images.is_empty();
                        (p.text, had)
                    }
                    Err(_) => (trimmed.to_string(), false),
                }
            } else {
                (trimmed.to_string(), false)
            };
            pending_interventions.push(intervention_text);
            let notice = if had_images {
                "📝 Intervention received (text only — image attachments are not supported during active runs). Will apply at next reasoning cycle."
            } else {
                "📝 Intervention received — will apply at next reasoning cycle"
            };
            let _ = live_send(live_tx, json!({"type":"progress","content":notice})).await;
        }
    }

    false
}
