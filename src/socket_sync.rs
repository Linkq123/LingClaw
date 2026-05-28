use serde_json::json;

use crate::{AppState, SessionSummary, WsTx, session_store::*, ws_send};

fn default_history_payload() -> serde_json::Value {
    json!({"type":"history","messages":[]})
}

fn default_view_state_payload() -> serde_json::Value {
    json!({"type":"view_state","show_tools":true,"show_reasoning":true,"show_react":true})
}

fn default_todos_state_payload() -> serde_json::Value {
    crate::todos::build_todos_state_event(&crate::todos::TodoSnapshot::default())
}

pub(crate) async fn send_existing_session_payloads(tx: &WsTx, state: &AppState, session_id: &str) {
    let config = state.config();
    let (name, history, view_state, todos_state, supports_image, usage) = {
        let sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get(session_id) {
            let model = session.effective_model(&config.model);
            let supports_image = config.model_supports_image(model);
            let usage = build_session_usage_payload(session);
            (
                session.name.clone(),
                build_history_payload_with_s3(session, config.s3.as_ref()),
                build_view_state_payload(session),
                crate::todos::build_todos_state_event(&session.todos),
                supports_image,
                usage,
            )
        } else {
            (
                "New Chat".to_string(),
                default_history_payload(),
                default_view_state_payload(),
                default_todos_state_payload(),
                false,
                json!({}),
            )
        }
    };

    let s3_available = config.s3.is_some();
    ws_send(
        tx,
        &json!({"type":"session","id":session_id,"name":name,"capabilities":{"image":supports_image,"s3":s3_available},"usage":usage}),
    )
    .await;
    ws_send(tx, &view_state).await;
    ws_send(tx, &todos_state).await;
    ws_send(tx, &history).await;
}

/// Build the session info payload including model capabilities.
pub(crate) fn build_session_info_payload(
    session_id: &str,
    name: &str,
    state: &AppState,
    effective_model: &str,
    usage: serde_json::Value,
) -> serde_json::Value {
    let config = state.config();
    let supports_image = config.model_supports_image(effective_model);
    let s3_available = config.s3.is_some();
    json!({"type":"session","id":session_id,"name":name,"capabilities":{"image":supports_image,"s3":s3_available},"usage":usage})
}

/// Build the usage sub-object for a session event.
pub(crate) fn build_session_usage_payload(session: &crate::Session) -> serde_json::Value {
    let (daily_input, daily_output) = crate::context::current_daily_token_usage(session);
    json!({
        "daily_input": daily_input,
        "daily_output": daily_output,
        "total_input": session.input_tokens,
        "total_output": session.output_tokens,
    })
}

pub(crate) fn build_session_list_payload(state: &AppState) -> serde_json::Value {
    let config = state.config();
    let mut summaries = list_saved_session_summaries_in_dir(&sessions_dir());

    if let Ok(sessions) = state.sessions.try_lock() {
        for session in sessions.values() {
            let already_listed = summaries.iter().any(|summary| summary.id == session.id);
            if already_listed {
                continue;
            }
            summaries.push(SessionSummary::from_session(session));
        }
    }

    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let mut seen_ids = std::collections::HashSet::new();
    let mut list = Vec::new();
    for summary in summaries {
        if !seen_ids.insert(summary.id.clone()) {
            continue;
        }
        let session = if summary.corrupt {
            None
        } else {
            load_session_from_disk(&summary.id)
        };
        list.push(summary.to_json(&config, session.as_ref()));
    }

    json!({"type":"session_list","sessions": list})
}

pub(crate) async fn broadcast_session_list_payload(state: &AppState) {
    let payload = build_session_list_payload(state);
    let clients = {
        let clients = state.session_clients.lock().await;
        clients
            .values()
            .map(|binding| binding.tx.clone())
            .collect::<Vec<_>>()
    };
    for tx in clients {
        ws_send(&tx, &payload).await;
    }
}

pub(crate) async fn send_command_refresh(
    tx: &WsTx,
    state: &AppState,
    session_id: &str,
    include_history: bool,
) {
    let config = state.config();
    let refresh_view_state = {
        let sessions = state.sessions.lock().await;
        sessions.get(session_id).map(|session| {
            let view_state = build_view_state_payload(session);
            let todos_state = crate::todos::build_todos_state_event(&session.todos);
            let history = if include_history {
                Some(build_history_payload_with_s3(session, config.s3.as_ref()))
            } else {
                None
            };
            (view_state, todos_state, history)
        })
    };

    if let Some((view_state, todos_state, history)) = refresh_view_state {
        ws_send(tx, &view_state).await;
        ws_send(tx, &todos_state).await;
        if let Some(history_payload) = history {
            ws_send(tx, &history_payload).await;
        }
    }
}
