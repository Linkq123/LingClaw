use serde_json::json;

use crate::{AppState, WsTx, session_store::*, ws_send};

fn default_history_payload() -> serde_json::Value {
    json!({"type":"history","messages":[]})
}

fn default_view_state_payload() -> serde_json::Value {
    json!({"type":"view_state","show_tools":true,"show_reasoning":true,"show_react":true})
}

pub(crate) async fn send_existing_session_payloads(tx: &WsTx, state: &AppState, session_id: &str) {
    let config = state.config();
    let (name, history, view_state, supports_image, usage) = {
        let sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get(session_id) {
            let model = session.effective_model(&config.model);
            let supports_image = config.model_supports_image(model);
            let usage = build_session_usage_payload(session);
            (
                session.name.clone(),
                build_history_payload_with_s3(session, config.s3.as_ref()),
                build_view_state_payload(session),
                supports_image,
                usage,
            )
        } else {
            (
                "New Chat".to_string(),
                default_history_payload(),
                default_view_state_payload(),
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
            let history = if include_history {
                Some(build_history_payload_with_s3(session, config.s3.as_ref()))
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
