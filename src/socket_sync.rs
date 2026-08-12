use serde_json::json;

use crate::{AppState, Config, SessionSummary, WsTx, session_store::*, ws_send};

pub(crate) struct ModelConfigurationPayloads {
    session_payloads: Vec<(String, serde_json::Value)>,
    group_payloads: Vec<(String, serde_json::Value)>,
}

fn default_history_payload() -> serde_json::Value {
    json!({"type":"history","messages":[]})
}

fn default_view_state_payload() -> serde_json::Value {
    json!({"type":"view_state","show_tools":true,"show_reasoning":true,"show_react":true})
}

fn default_todos_state_payload() -> serde_json::Value {
    crate::todos::build_todos_state_event(&crate::todos::TodoSnapshot::default())
}

#[cfg(not(test))]
async fn attach_plan_history(
    tx: &WsTx,
    state: &AppState,
    session_id: &str,
    mut payload: serde_json::Value,
) -> serde_json::Value {
    match crate::storage::Database::global() {
        Ok(database) => match database.load_plan_history(session_id).await {
            Ok(plans) if !plans.is_empty() => payload["plans"] = json!(plans),
            Ok(_) => {}
            Err(error) => {
                eprintln!("ERROR: failed to load plan history for {session_id}: {error}");
                crate::send_storage_status(tx, state).await;
            }
        },
        Err(error) => {
            eprintln!("ERROR: LingClaw storage is unavailable while loading plans: {error}");
        }
    }
    payload
}

#[cfg(test)]
async fn attach_plan_history(
    _tx: &WsTx,
    _state: &AppState,
    _session_id: &str,
    payload: serde_json::Value,
) -> serde_json::Value {
    payload
}

pub(crate) async fn send_existing_session_payloads(tx: &WsTx, state: &AppState, session_id: &str) {
    let model_status_guard = crate::CONFIG_FILE_LOCK.read().await;
    let (config, config_revision) = state.config_snapshot_with_revision();
    let (
        name,
        history,
        view_state,
        todos_state,
        effective_model,
        effort,
        supports_image,
        model_override_present,
        model_override_configured,
        effective_model_configured,
        usage,
    ) = {
        let sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get(session_id) {
            let (model, model_override_configured, effective_model_configured) =
                session.model_configuration(&config);
            let supports_image = config.model_supports_image(&model);
            let effort = config.normalize_model_effort(&model, &session.think_level);
            let usage = build_session_usage_payload(session);
            (
                session.name.clone(),
                build_history_payload_with_s3(session, config.s3.as_ref()),
                build_view_state_payload(session),
                crate::todos::build_todos_state_event(&session.todos),
                model,
                effort,
                supports_image,
                session.model_override.is_some(),
                model_override_configured,
                effective_model_configured,
                usage,
            )
        } else {
            (
                "New Chat".to_string(),
                default_history_payload(),
                default_view_state_payload(),
                default_todos_state_payload(),
                config.model.clone(),
                config.normalize_model_effort(&config.model, "auto"),
                false,
                false,
                false,
                config.explicit_primary_model_configured,
                json!({}),
            )
        }
    };

    let s3_available = config.s3.is_some();
    let s3_config_id = config.s3.as_ref().map(crate::image_uploads::s3_config_id);
    let session_payload = json!({"type":"session","id":session_id,"name":name,"model":effective_model,"effort":effort,"explicitPrimaryModelConfigured":config.explicit_primary_model_configured,"modelOverridePresent":model_override_present,"modelOverrideConfigured":model_override_configured,"effectiveModelConfigured":effective_model_configured,"configRevision":config_revision,"capabilities":{"image":supports_image,"s3":s3_available,"s3_config_id":s3_config_id},"usage":usage});
    drop(model_status_guard);
    let history = attach_plan_history(tx, state, session_id, history).await;

    ws_send(tx, &session_payload).await;
    ws_send(tx, &view_state).await;
    ws_send(tx, &todos_state).await;
    ws_send(tx, &history).await;
    // Serialize the initial discovery payload with Group feature transitions.
    // The runtime Config can change while plan history is being attached, so
    // consult the latest value while holding the shared feature gate.
    let group_feature_guard = crate::session_group::group_feature_gate().read().await;
    let groups_enabled = state.config().enable_groups;
    ws_send(
        tx,
        &json!({
            "type": "feature_status",
            "features": { "groups": groups_enabled },
        }),
    )
    .await;
    if groups_enabled {
        match crate::build_group_list_payload() {
            Ok(payload) => {
                ws_send(tx, &payload).await;
            }
            Err(_) => crate::send_storage_status(tx, state).await,
        }
    }
    drop(group_feature_guard);
}

/// Build the session info payload including model capabilities.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_session_info_payload(
    session_id: &str,
    name: &str,
    config: &Config,
    effective_model: &str,
    effort: &str,
    model_override_present: bool,
    model_override_configured: bool,
    effective_model_configured: bool,
    config_revision: u64,
    usage: serde_json::Value,
) -> serde_json::Value {
    let supports_image = config.model_supports_image(effective_model);
    let s3_available = config.s3.is_some();
    let s3_config_id = config.s3.as_ref().map(crate::image_uploads::s3_config_id);
    json!({"type":"session","id":session_id,"name":name,"model":effective_model,"effort":effort,"explicitPrimaryModelConfigured":config.explicit_primary_model_configured,"modelOverridePresent":model_override_present,"modelOverrideConfigured":model_override_configured,"effectiveModelConfigured":effective_model_configured,"configRevision":config_revision,"capabilities":{"image":supports_image,"s3":s3_available,"s3_config_id":s3_config_id},"usage":usage})
}

struct SessionModelConfigurationSnapshot<'a> {
    model: &'a str,
    effort: &'a str,
    model_override_present: bool,
    model_override_configured: bool,
    effective_model_configured: bool,
}

fn build_session_model_configuration_payload(
    session_id: &str,
    config: &Config,
    snapshot: SessionModelConfigurationSnapshot<'_>,
    config_revision: u64,
) -> serde_json::Value {
    json!({
        "type": "session_model_configuration",
        "id": session_id,
        "model": snapshot.model,
        "effort": snapshot.effort,
        "explicitPrimaryModelConfigured": config.explicit_primary_model_configured,
        "modelOverridePresent": snapshot.model_override_present,
        "modelOverrideConfigured": snapshot.model_override_configured,
        "effectiveModelConfigured": snapshot.effective_model_configured,
        "configRevision": config_revision,
        "capabilities": {
            "image": config.model_supports_image(snapshot.model),
            "s3": config.s3.is_some(),
            "s3_config_id": config.s3.as_ref().map(crate::image_uploads::s3_config_id),
        },
    })
}

pub(crate) async fn collect_model_configuration_payloads(
    state: &AppState,
    config: &Config,
    config_revision: u64,
) -> ModelConfigurationPayloads {
    let session_ids = {
        let clients = state.session_clients.lock().await;
        clients.keys().cloned().collect::<Vec<_>>()
    };
    let session_payloads = {
        let sessions = state.sessions.lock().await;
        session_ids
            .into_iter()
            .map(|session_id| {
                let (
                    model,
                    effort,
                    model_override_present,
                    model_override_configured,
                    effective_model_configured,
                ) = sessions
                    .get(&session_id)
                    .map(|session| {
                        let (model, model_override_configured, effective_model_configured) =
                            session.model_configuration(config);
                        let effort = config.normalize_model_effort(&model, &session.think_level);
                        (
                            model,
                            effort,
                            session.model_override.is_some(),
                            model_override_configured,
                            effective_model_configured,
                        )
                    })
                    .unwrap_or_else(|| {
                        (
                            config.model.clone(),
                            config.normalize_model_effort(&config.model, "auto"),
                            false,
                            false,
                            config.explicit_primary_model_configured,
                        )
                    });
                let payload = build_session_model_configuration_payload(
                    &session_id,
                    config,
                    SessionModelConfigurationSnapshot {
                        model: &model,
                        effort: &effort,
                        model_override_present,
                        model_override_configured,
                        effective_model_configured,
                    },
                    config_revision,
                );
                (session_id, payload)
            })
            .collect()
    };
    let group_payloads = crate::session_control::collect_group_model_configuration_payloads(
        state,
        config,
        config_revision,
    )
    .await;
    ModelConfigurationPayloads {
        session_payloads,
        group_payloads,
    }
}

pub(crate) async fn send_model_configuration_payloads(
    state: &AppState,
    payloads: ModelConfigurationPayloads,
) {
    let ModelConfigurationPayloads {
        session_payloads,
        group_payloads,
    } = payloads;
    let session_sends = session_payloads
        .into_iter()
        .map(|(session_id, payload)| async move {
            crate::send_session_client_event(state, &session_id, payload).await;
        });
    tokio::join!(
        futures::future::join_all(session_sends),
        crate::session_control::send_group_model_configuration_payloads(state, group_payloads),
    );
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

pub(crate) async fn build_session_list_payload(
    state: &AppState,
) -> Result<serde_json::Value, String> {
    let config = state.config();
    let mut summaries = list_saved_session_summaries_result(&sessions_dir())?;

    if let Ok(sessions) = state.sessions.try_lock() {
        for session in sessions.values() {
            if let Some(summary) = summaries
                .iter_mut()
                .find(|summary| crate::session_ids_match(&summary.id, &session.id))
            {
                *summary = SessionSummary::from_session(session);
            } else {
                summaries.push(SessionSummary::from_session(session));
            }
        }
    }

    sort_session_summaries(&mut summaries);

    let mut seen_ids = std::collections::HashSet::new();
    let mut list = Vec::new();
    let mut unique_summaries = Vec::new();
    for summary in summaries {
        let dedupe_id = if cfg!(windows) {
            summary.id.to_ascii_lowercase()
        } else {
            summary.id.clone()
        };
        if !seen_ids.insert(dedupe_id) {
            continue;
        }
        unique_summaries.push(summary);
    }

    list.extend(
        futures::future::join_all(unique_summaries.into_iter().map(|summary| {
            let config = config.clone();
            async move {
                let available = working_directory_available(&summary.working_directory).await;
                summary.to_json(&config, None, available)
            }
        }))
        .await,
    );

    Ok(json!({"type":"session_list","sessions": list}))
}

pub(crate) async fn broadcast_session_list_payload(state: &AppState) {
    let payload = match build_session_list_payload(state).await {
        Ok(payload) => payload,
        Err(_) => {
            #[cfg(not(test))]
            crate::broadcast_storage_status(state).await;
            return;
        }
    };
    let session_ids = {
        let clients = state.session_clients.lock().await;
        clients.keys().cloned().collect::<Vec<_>>()
    };
    for session_id in session_ids {
        crate::send_session_client_event(state, &session_id, payload.clone()).await;
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
            let history_payload = attach_plan_history(tx, state, session_id, history_payload).await;
            ws_send(tx, &history_payload).await;
        }
    }
}
