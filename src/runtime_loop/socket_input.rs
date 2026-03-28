use super::*;

use serde_json::json;

pub(crate) enum IdleSocketInputAction {
    Continue,
    StartAgent,
    Break,
}

pub(crate) async fn resolve_or_create_socket_session(
    state: &Arc<AppState>,
    tx: &WsTx,
    requested_id: Option<&str>,
    connection_id: u64,
) -> String {
    let mut claimed = if let Some(req_id) = requested_id {
        claim_requested_session(req_id, state, connection_id).await
    } else {
        None
    };

    if claimed.is_none() && requested_id.is_none() {
        match try_claim_session(MAIN_SESSION_ID, state, connection_id).await {
            ClaimSessionResult::Claimed(id) => {
                claimed = Some(id);
            }
            ClaimSessionResult::InUse | ClaimSessionResult::NotFound => {
                let saved_ids = list_recoverable_saved_session_ids();
                for cid in &saved_ids {
                    match try_claim_session(cid, state, connection_id).await {
                        ClaimSessionResult::Claimed(id) => {
                            claimed = Some(id);
                            break;
                        }
                        ClaimSessionResult::InUse | ClaimSessionResult::NotFound => continue,
                    }
                }
            }
        }
    }

    if let Some(id) = claimed {
        send_existing_session_payloads(tx, state, &id).await;
        id
    } else {
        let mut session = Session::new();
        let sys = build_system_prompt(
            &state.config,
            &session.workspace,
            session.effective_model(&state.config.model),
            false,
            &session.disabled_system_skills,
        );
        session.messages.push(sys);
        let current_session_id = session.id.clone();
        if let Err(error) = save_session_to_disk(&session).await {
            eprintln!(
                "Warning: failed to persist new session {} on creation: {error}; keeping in memory",
                current_session_id
            );
        }
        {
            let mut active = state.active_connections.lock().await;
            let mut sessions = state.sessions.lock().await;
            sessions.insert(current_session_id.clone(), session);
            active.insert(current_session_id.clone(), connection_id);
        }
        send_new_session_payload(tx, state, &current_session_id).await;
        current_session_id
    }
}

pub(crate) async fn handle_idle_socket_input(
    text: String,
    current_session_id: &mut String,
    current_session_ref: &Arc<Mutex<String>>,
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
        if let Some(result) = cmd_result {
            send_command_refresh(tx, state, current_session_id, result.refresh_history).await;

            ws_send(
                tx,
                &json!({"type":result.response_type,"content":result.response}),
            )
            .await;

            if let Some(new_id) = result.new_session_id {
                unbind_session_connection_if_matches(state, current_session_id, connection_id)
                    .await;
                state.live_rounds.lock().await.remove(current_session_id);
                *current_session_id = new_id.clone();
                bind_session_connection(state, current_session_id, connection_id, tx, true).await;
                {
                    let mut active_id = current_session_ref.lock().await;
                    *active_id = current_session_id.clone();
                }
                send_session_switched_payloads(tx, state, &new_id).await;
            }
            if result.sessions_changed {
                send_sessions_list(tx, state, current_session_id).await;
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

    {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(current_session_id) {
            session.messages.push(ChatMessage {
                role: "user".into(),
                content: Some(text),
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
                tool_calls: None,
                tool_call_id: None,
                timestamp: Some(now_epoch()),
            });
        }
        session.updated_at = now_epoch();
    }
}

pub(super) async fn drain_busy_socket_messages(
    inbound_rx: &mut mpsc::Receiver<String>,
    pending_interventions: &mut Vec<String>,
    live_tx: &LiveTx,
    run_cancel: &CancellationToken,
) -> bool {
    while let Ok(msg) = inbound_rx.try_recv() {
        let trimmed = msg.trim();
        if trimmed.eq_ignore_ascii_case("/stop") {
            run_cancel.cancel();
            return true;
        }
        if !trimmed.is_empty() && !trimmed.starts_with('/') {
            pending_interventions.push(trimmed.to_string());
            let _ = live_send(
                live_tx,
                json!({"type":"progress","content":"📝 Intervention received — will apply at next reasoning cycle"}),
            )
            .await;
        }
    }

    false
}
