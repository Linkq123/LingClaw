use serde_json::json;
use std::collections::HashSet;

use crate::session_store::{
    build_global_today_usage, build_session_status, list_saved_session_ids,
    load_session_snapshot_from_path, resolve_session_target, sessions_dir,
};
use crate::{session_workspace_path, AppState, Session, MAIN_SESSION_ID};

fn load_saved_sessions_excluding(excluded_ids: &HashSet<String>) -> Vec<Session> {
    let mut sessions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(sessions_dir()) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let Ok(data) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(raw_session) = serde_json::from_str::<Session>(&data) else {
                continue;
            };
            if excluded_ids.contains(&raw_session.id) {
                continue;
            }

            let Some(session) = load_session_snapshot_from_path(&path) else {
                continue;
            };

            sessions.push(session);
        }
    }
    sessions
}

pub(crate) async fn gather_global_today_usage(state: &AppState) -> String {
    let mut sessions_snapshot = state.sessions.lock().await.clone();
    let in_memory_ids = sessions_snapshot.keys().cloned().collect::<HashSet<_>>();
    for session in load_saved_sessions_excluding(&in_memory_ids) {
        sessions_snapshot.insert(session.id.clone(), session);
    }
    build_global_today_usage(&sessions_snapshot)
}

/// Gather all sessions info for /sessions command and list_sessions tool.
pub(crate) async fn gather_sessions_status(state: &AppState) -> String {
    let active_ids: HashSet<String> = state
        .active_connections
        .lock()
        .await
        .keys()
        .cloned()
        .collect();

    let mut all_sessions = {
        let sessions = state.sessions.lock().await;
        sessions.values().cloned().collect::<Vec<_>>()
    };
    let loaded_ids = all_sessions
        .iter()
        .map(|session| session.id.clone())
        .collect::<HashSet<_>>();
    all_sessions.extend(load_saved_sessions_excluding(&loaded_ids));

    all_sessions.sort_by(|left, right| {
        active_ids
            .contains(&right.id)
            .cmp(&active_ids.contains(&left.id))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.id.cmp(&right.id))
    });

    let lines = all_sessions
        .iter()
        .map(|session| {
            let status = if active_ids.contains(&session.id) {
                "active"
            } else {
                "saved"
            };
            let status_block = build_session_status(session, &state.config)
                .replace('\n', "\n    ");
            format!("  {}  {}  [{}]\n    {}", session.id, session.name, status, status_block)
        })
        .collect::<Vec<_>>();

    if lines.is_empty() {
        "No sessions.".to_string()
    } else {
        format!("Sessions ({}):\n{}", lines.len(), lines.join("\n"))
    }
}

/// Delete a session by ID. Returns a status message.
pub(crate) async fn delete_session_by_id(target: &str, state: &AppState) -> String {
    let target = target.trim();
    if target == MAIN_SESSION_ID {
        return "Cannot delete the main session.".to_string();
    }
    if target.contains('/') || target.contains('\\') || target.contains("..") {
        return "Invalid session ID.".to_string();
    }

    let known_ids: HashSet<String> = {
        let mut ids = {
            let sessions = state.sessions.lock().await;
            sessions.keys().cloned().collect::<HashSet<_>>()
        };
        ids.extend(list_saved_session_ids());
        ids
    };

    let resolved_id = match resolve_session_target(target, &known_ids) {
        Ok(id) => id,
        Err(message) => return message,
    };

    if resolved_id == MAIN_SESSION_ID {
        return "Cannot delete the main session.".to_string();
    }

    let session_in_use = {
        let active = state.active_connections.lock().await;
        if active.contains_key(&resolved_id) {
            true
        } else {
            drop(active);
            state.session_clients.lock().await.contains_key(&resolved_id)
        }
    };
    if session_in_use {
        return format!("Session '{}' is currently in use.", resolved_id);
    }

    let removed_session = {
        let mut sessions = state.sessions.lock().await;
        sessions.remove(&resolved_id)
    };

    let path = sessions_dir().join(format!("{resolved_id}.json"));
    let existed_on_disk = path.exists();
    if existed_on_disk {
        if let Err(e) = std::fs::remove_file(&path) {
            if let Some(session) = removed_session {
                state
                    .sessions
                    .lock()
                    .await
                    .insert(resolved_id.clone(), session);
            }
            return format!("Failed to delete session file: {e}");
        }
    }

    if removed_session.is_none() && !existed_on_disk {
        return format!("Session '{}' not found.", target);
    }

    // Optionally clean up workspace directory
    let ws_path = session_workspace_path(&resolved_id);
    if let Some(session_dir) = ws_path.parent() {
        if session_dir.exists() {
            let _ = std::fs::remove_dir_all(session_dir);
        }
    }

    format!("Deleted session '{}'.", resolved_id)
}

/// Admin tool definitions for the LLM (OpenAI format).
pub(crate) fn admin_tool_definitions_openai() -> Vec<serde_json::Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "list_sessions",
                "description": "List all sessions with their model, context usage, max_tokens, and status",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "delete_session",
                "description": "Delete a session by its ID. Cannot delete the main session or an active session.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "The session ID to delete"
                        }
                    },
                    "required": ["session_id"]
                }
            }
        }),
    ]
}

/// Admin tool definitions for the LLM (Anthropic format).
pub(crate) fn admin_tool_definitions_anthropic() -> Vec<serde_json::Value> {
    vec![
        json!({
            "name": "list_sessions",
            "description": "List all sessions with their model, context usage, max_tokens, and status",
            "input_schema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
        json!({
            "name": "delete_session",
            "description": "Delete a session by its ID. Cannot delete the main session or an active session.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The session ID to delete"
                    }
                },
                "required": ["session_id"]
            }
        }),
    ]
}

/// Execute an admin tool call. Returns the tool result string.
pub(crate) async fn execute_admin_tool(name: &str, args_str: &str, state: &AppState) -> String {
    match name {
        "list_sessions" => gather_sessions_status(state).await,
        "delete_session" => {
            let args: serde_json::Value = serde_json::from_str(args_str).unwrap_or_default();
            let session_id = args["session_id"].as_str().unwrap_or_default();
            if session_id.is_empty() {
                return "Error: session_id is required.".to_string();
            }
            delete_session_by_id(session_id, state).await
        }
        _ => format!("Unknown admin tool: {name}"),
    }
}

pub(crate) fn is_admin_tool(name: &str) -> bool {
    matches!(name, "list_sessions" | "delete_session")
}
