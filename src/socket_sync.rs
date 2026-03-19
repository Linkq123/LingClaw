use serde_json::json;
use std::collections::HashMap;

use crate::{session_store::*, ws_send, AppState, WsTx};

fn default_history_payload() -> serde_json::Value {
    json!({"type":"history","messages":[]})
}

fn default_view_state_payload() -> serde_json::Value {
    json!({"type":"view_state","show_tools":true,"show_reasoning":true,"show_react":true})
}

pub(crate) async fn send_existing_session_payloads(tx: &WsTx, state: &AppState, session_id: &str) {
    let (name, history, view_state) = {
        let sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get(session_id) {
            (
                session.name.clone(),
                build_history_payload(session),
                build_view_state_payload(session),
            )
        } else {
            (
                "New Chat".to_string(),
                default_history_payload(),
                default_view_state_payload(),
            )
        }
    };

    ws_send(tx, &json!({"type":"session","id":session_id,"name":name})).await;
    ws_send(tx, &view_state).await;
    ws_send(tx, &history).await;
}

pub(crate) async fn send_new_session_payload(tx: &WsTx, state: &AppState, session_id: &str) {
    ws_send(
        tx,
        &json!({"type":"session","id":session_id,"name":"New Chat"}),
    )
    .await;

    let view_state = {
        let sessions = state.sessions.lock().await;
        sessions.get(session_id).map(build_view_state_payload)
    };
    if let Some(view_state) = view_state {
        ws_send(tx, &view_state).await;
    }
}

pub(crate) async fn send_command_refresh(
    tx: &WsTx,
    state: &AppState,
    session_id: &str,
    include_history: bool,
) {
    let refresh_view_state = {
        let sessions = state.sessions.lock().await;
        sessions.get(session_id).map(|session| {
            let view_state = build_view_state_payload(session);
            let history = if include_history {
                Some(build_history_payload(session))
            } else {
                None
            };
            (view_state, history)
        })
    };

    if let Some((view_state, history)) = refresh_view_state {
        ws_send(tx, &view_state).await;
        if let Some(history_payload) = history {
            ws_send(tx, &history_payload).await;
        }
    }
}

pub(crate) async fn send_session_switched_payloads(tx: &WsTx, state: &AppState, session_id: &str) {
    let (name, view_state, history) = {
        let sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get(session_id) {
            (
                session.name.clone(),
                build_view_state_payload(session),
                Some(build_history_payload(session)),
            )
        } else {
            ("New Chat".to_string(), default_view_state_payload(), None)
        }
    };

    ws_send(
        tx,
        &json!({"type":"session_switched","id":session_id,"name":name}),
    )
    .await;
    ws_send(tx, &view_state).await;
    if let Some(history) = history {
        ws_send(tx, &history).await;
    }
}

pub(crate) async fn send_sessions_list(tx: &WsTx, state: &AppState, active_id: &str) {
    let in_mem: HashMap<String, serde_json::Value> = {
        let sessions = state.sessions.lock().await;
        sessions
            .iter()
            .map(|(id, session)| {
                let msg_count = sanitized_non_system_message_count(session);
                (
                    id.clone(),
                    json!({
                        "id": id,
                        "name": session.name,
                        "messages": msg_count,
                        "created_at": session.created_at,
                        "updated_at": session.updated_at,
                        "active": id == active_id,
                    }),
                )
            })
            .collect()
    };

    let mut all = list_saved_session_summaries();
    for item in &mut all {
        let id = item["id"].as_str().unwrap_or_default().to_string();
        if let Some(mem) = in_mem.get(&id) {
            *item = mem.clone();
        } else {
            item["active"] = json!(id == active_id);
        }
    }
    for (id, val) in &in_mem {
        if !all.iter().any(|session| session["id"].as_str() == Some(id)) {
            all.push(val.clone());
        }
    }
    all.sort_by(|a, b| {
        let b_ts = b["updated_at"].as_u64().unwrap_or(0);
        let a_ts = a["updated_at"].as_u64().unwrap_or(0);
        b_ts.cmp(&a_ts)
    });
    all.retain(|session| {
        session["active"].as_bool() == Some(true)
            || session["messages"].as_u64().unwrap_or(0) > 0
            || session["corrupt"].as_bool() == Some(true)
    });
    ws_send(tx, &json!({"type":"sessions_list","sessions":all})).await;
}
