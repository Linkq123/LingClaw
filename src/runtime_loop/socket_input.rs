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
                let name = {
                    let sessions = state.sessions.lock().await;
                    sessions
                        .get(current_session_id.as_str())
                        .map(|s| s.name.clone())
                        .unwrap_or_else(|| "Main".to_string())
                };
                ws_send(
                    tx,
                    &json!({"type":"session","id":current_session_id,"name":name}),
                )
                .await;
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
