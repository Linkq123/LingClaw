use std::path::Path;

use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::{
    build_system_prompt, default_show_react, default_show_reasoning, default_show_tools, now_epoch,
    prompts, providers,
    session_admin::{delete_session_by_id, gather_global_today_usage, gather_sessions_status},
    session_store::{build_session_status, build_usage_report, save_session_to_disk, sessions_dir},
    tools, truncate, try_claim_session, ws_send, AppState, ChatMessage, ClaimSessionResult,
    Session, WsTx, MAIN_SESSION_ID,
};

// ── Chat Commands ────────────────────────────────────────────────────────────

pub(crate) struct CommandResult {
    pub(crate) response: String,
    pub(crate) response_type: &'static str,
    pub(crate) new_session_id: Option<String>,
    pub(crate) sessions_changed: bool,
    pub(crate) refresh_history: bool,
}

pub(crate) fn command_result(
    response: impl Into<String>,
    response_type: &'static str,
    new_session_id: Option<String>,
    sessions_changed: bool,
) -> CommandResult {
    CommandResult {
        response: response.into(),
        response_type,
        new_session_id,
        sessions_changed,
        refresh_history: false,
    }
}

pub(crate) fn command_result_with_history(
    response: impl Into<String>,
    response_type: &'static str,
    new_session_id: Option<String>,
    sessions_changed: bool,
) -> CommandResult {
    CommandResult {
        refresh_history: true,
        ..command_result(response, response_type, new_session_id, sessions_changed)
    }
}

async fn persist_session_update<T, Capture, Apply, Restore>(
    state: &AppState,
    current_session_id: &str,
    capture: Capture,
    apply: Apply,
    restore: Restore,
) -> Result<(), String>
where
    Capture: FnOnce(&Session) -> T,
    Apply: FnOnce(&mut Session),
    Restore: FnOnce(&mut Session, T),
{
    let (captured, session_to_save) = {
        let mut sessions = state.sessions.lock().await;
        let session = sessions
            .get_mut(current_session_id)
            .ok_or_else(|| "Session not found".to_string())?;
        let captured = capture(session);
        apply(session);
        session.updated_at = now_epoch();
        (captured, session.clone())
    };

    if let Err(err) = save_session_to_disk(&session_to_save).await {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(current_session_id) {
            restore(session, captured);
        }
        return Err(err);
    }

    Ok(())
}

fn parse_toggle_value(arg: &str, command_name: &str) -> Result<bool, String> {
    match arg.to_lowercase().as_str() {
        "on" | "true" | "1" => Ok(true),
        "off" | "false" | "0" => Ok(false),
        _ => Err(format!(
            "Invalid value: {arg}\nUsage: /{command_name} <on|off>"
        )),
    }
}

async fn append_daily_memory_entry(
    memory_path: &Path,
    today: &str,
    local_time: &str,
    summary: &str,
) -> std::io::Result<()> {
    let entry = format!("\n\n---\n\n## {local_time} Local\n\n{}", summary.trim());
    let initial_content = format!("# {today}\n{entry}");

    match tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(memory_path)
        .await
    {
        Ok(mut file) => file.write_all(initial_content.as_bytes()).await,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut file = tokio::fs::OpenOptions::new()
                .append(true)
                .open(memory_path)
                .await?;
            file.write_all(entry.as_bytes()).await
        }
        Err(err) => Err(err),
    }
}

async fn reset_session_context_and_persist(
    state: &AppState,
    current_session_id: &str,
) -> Result<(), String> {
    persist_session_update(
        state,
        current_session_id,
        |session| {
            (
                session.messages.clone(),
                session.tool_calls_count,
                session.updated_at,
            )
        },
        |session| {
            let model = session.effective_model(&state.config.model).to_string();
            let is_main = session.is_main();
            let sys = build_system_prompt(&state.config, &session.workspace, &model, is_main);
            session.messages = vec![sys];
            session.tool_calls_count = 0;
        },
        |session, (messages, tool_calls_count, updated_at)| {
            session.messages = messages;
            session.tool_calls_count = tool_calls_count;
            session.updated_at = updated_at;
        },
    )
    .await
}

async fn delete_session_artifacts(session_id: &str) {
    let path = sessions_dir().join(format!("{session_id}.json"));
    if tokio::fs::try_exists(&path).await.unwrap_or(false) {
        let _ = tokio::fs::remove_file(&path).await;
    }

    if let Some(session_dir) = crate::session_workspace_path(session_id)
        .parent()
        .map(Path::to_path_buf)
    {
        if tokio::fs::try_exists(&session_dir).await.unwrap_or(false) {
            let _ = tokio::fs::remove_dir_all(session_dir).await;
        }
    }
}

async fn handle_new_command(
    current_session_id: &str,
    state: &AppState,
    tx: &WsTx,
    cancel: &CancellationToken,
) -> Option<CommandResult> {
    let (conversation_text, workspace, model_str) = {
        let sessions = state.sessions.lock().await;
        let session = match sessions.get(current_session_id) {
            Some(s) => s,
            None => return Some(command_result("Session not found", "system", None, false)),
        };
        let mut lines = Vec::new();
        for msg in &session.messages {
            match msg.role.as_str() {
                "user" => {
                    if let Some(c) = &msg.content {
                        lines.push(format!("User: {c}"));
                    }
                }
                "assistant" => {
                    if let Some(c) = &msg.content {
                        if !c.is_empty() {
                            lines.push(format!("Assistant: {c}"));
                        }
                    }
                }
                _ => {}
            }
        }
        (
            lines.join("\n"),
            session.workspace.clone(),
            session.effective_model(&state.config.model).to_string(),
        )
    };

    if conversation_text.is_empty() {
        match reset_session_context_and_persist(state, current_session_id).await {
            Ok(()) => {
                return Some(command_result_with_history(
                    "Context cleared.",
                    "system",
                    None,
                    true,
                ));
            }
            Err(err) if err == "Session not found" => {
                return Some(command_result(err, "system", None, false));
            }
            Err(err) => {
                return Some(command_result(
                    format!("Failed to persist cleared context: {err}"),
                    "error",
                    None,
                    false,
                ));
            }
        }
    }

    if !ws_send(
        tx,
        &json!({
            "type": "progress",
            "content": "Compressing conversation..."
        }),
    )
    .await
    {
        return None;
    }

    let compress_prompt = vec![
        ChatMessage {
            role: "system".into(),
            content: Some("You are a conversation summarizer. Compress the following conversation into a concise markdown summary. Keep key decisions, code changes, problems solved, and important context. Use bullet points. Write in the same language as the conversation. Do NOT wrap in code blocks.".into()),
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(truncate(&conversation_text, 60_000)),
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(now_epoch()),
        },
    ];
    let resolved = state.config.resolve_model(&model_str);
    let summary = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            return Some(command_result(
                "Shutdown: compression skipped, context unchanged.",
                "system",
                None,
                false,
            ));
        }
        result = providers::call_llm_simple(&state.http, &resolved, &compress_prompt) => {
            match result {
                Ok(s) => s,
                Err(e) => {
                    return Some(command_result(
                        format!("Failed to compress conversation: {e}"),
                        "system",
                        None,
                        false,
                    ));
                }
            }
        }
    };

    if !ws_send(
        tx,
        &json!({
            "type": "progress",
            "content": "Compression complete. Writing memory..."
        }),
    )
    .await
    {
        return None;
    }

    let local_snapshot = prompts::current_local_snapshot();
    let today = local_snapshot.today();
    let memory_dir = workspace.join("memory");
    tokio::fs::create_dir_all(&memory_dir).await.ok();
    let memory_path = memory_dir.join(format!("{today}.md"));

    let write_result =
        append_daily_memory_entry(&memory_path, &today, &local_snapshot.hhmm(), &summary).await;

    if let Err(e) = write_result {
        return Some(command_result(
            format!("Failed to write memory: {e}"),
            "system",
            None,
            false,
        ));
    }

    match reset_session_context_and_persist(state, current_session_id).await {
        Ok(()) => {}
        Err(err) if err == "Session not found" => {
            return Some(command_result(err, "system", None, false));
        }
        Err(err) => {
            return Some(command_result(
                format!("Failed to persist cleared context: {err}"),
                "error",
                None,
                false,
            ));
        }
    }

    Some(command_result_with_history(
        format!("Conversation compressed and saved to memory/{today}.md. Context cleared."),
        "success",
        None,
        true,
    ))
}

async fn handle_session_new_command(current_session_id: &str, state: &AppState) -> CommandResult {
    let snapshot = {
        let sessions = state.sessions.lock().await;
        sessions.get(current_session_id).cloned()
    };
    if let Some(ref s) = snapshot {
        if s.messages.len() > 1 {
            match save_session_to_disk(s).await {
                Ok(()) => {
                    state.sessions.lock().await.remove(current_session_id);
                }
                Err(e) => {
                    eprintln!(
                        "Warning: failed to save session {} before /session_new: {e}; keeping in memory",
                        s.id
                    );
                }
            }
        } else {
            state.sessions.lock().await.remove(current_session_id);
            delete_session_artifacts(current_session_id).await;
        }
    }

    let mut session = Session::new();
    let model = session.effective_model(&state.config.model).to_string();
    let sys = build_system_prompt(&state.config, &session.workspace, &model, false);
    session.messages.push(sys);
    let new_id = session.id.clone();
    if let Err(err) = save_session_to_disk(&session).await {
        eprintln!(
            "Warning: failed to persist new session {} during /session_new: {err}; keeping in memory",
            new_id
        );
    }
    state.sessions.lock().await.insert(new_id.clone(), session);

    command_result("A new journey begins.", "system", Some(new_id), true)
}

async fn handle_switch_command(
    arg: &str,
    current_session_id: &str,
    connection_id: u64,
    state: &AppState,
) -> CommandResult {
    if arg.is_empty() {
        return command_result("Usage: /switch <session_id>", "system", None, false);
    }
    let target = arg.to_string();
    if target == current_session_id {
        return command_result("Already on this session.", "system", None, false);
    }

    let snapshot = {
        let sessions = state.sessions.lock().await;
        sessions.get(current_session_id).cloned()
    };
    let should_delete_current_if_switch_succeeds = snapshot
        .as_ref()
        .is_some_and(|session| session.messages.len() <= 1);
    if let Some(ref s) = snapshot {
        if s.messages.len() > 1 {
            if let Err(e) = save_session_to_disk(s).await {
                eprintln!(
                    "Warning: failed to save session {} before /switch: {e}; keeping in memory",
                    s.id
                );
                return command_result(
                    "Failed to save current session; switch cancelled to avoid data loss.",
                    "system",
                    None,
                    false,
                );
            }
        }
    }

    match try_claim_session(&target, state, connection_id).await {
        ClaimSessionResult::Claimed(id) => {
            state.sessions.lock().await.remove(current_session_id);
            if should_delete_current_if_switch_succeeds {
                delete_session_artifacts(current_session_id).await;
            }
            command_result(
                format!("Loaded session {}", &id[..12.min(id.len())]),
                "system",
                Some(id),
                true,
            )
        }
        ClaimSessionResult::InUse => command_result(
            format!(
                "Session '{}' is in use by another connection.",
                &target[..12.min(target.len())]
            ),
            "system",
            None,
            false,
        ),
        ClaimSessionResult::NotFound => command_result(
            format!("Session '{}' not found.", &target[..12.min(target.len())]),
            "system",
            None,
            false,
        ),
    }
}

async fn handle_model_command(
    arg: &str,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    if arg.is_empty() {
        let sessions = state.sessions.lock().await;
        let model = sessions
            .get(current_session_id)
            .map(|s| s.effective_model(&state.config.model))
            .unwrap_or(&state.config.model)
            .to_string();
        let current = state
            .config
            .canonical_model_ref(&model)
            .unwrap_or(model.clone());
        let available = state.config.available_models();
        let list = available
            .iter()
            .map(|m| {
                if m == &current {
                    format!("  * {m} (current)")
                } else {
                    format!("    {m}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        return command_result(
            format!("Available models:\n{list}\n\nUse /model <name> to switch."),
            "system",
            None,
            false,
        );
    }

    let canonical = match state.config.canonical_model_ref(arg) {
        Ok(value) => value,
        Err(err) => return command_result(err, "error", None, false),
    };
    match persist_session_update(
        state,
        current_session_id,
        |session| (session.model_override.clone(), session.updated_at),
        |session| {
            session.model_override = Some(canonical.clone());
        },
        |session, (model_override, updated_at)| {
            session.model_override = model_override;
            session.updated_at = updated_at;
        },
    )
    .await
    {
        Ok(()) => command_result(
            format!("Model switched to: {canonical}"),
            "system",
            None,
            true,
        ),
        Err(err) if err == "Session not found" => command_result(err, "system", None, false),
        Err(err) => command_result(
            format!("Failed to persist model switch: {err}"),
            "error",
            None,
            false,
        ),
    }
}

async fn handle_status_command(current_session_id: &str, state: &AppState) -> CommandResult {
    let sessions = state.sessions.lock().await;
    match sessions.get(current_session_id) {
        Some(session) => command_result(
            build_session_status(session, &state.config),
            "system",
            None,
            false,
        ),
        None => command_result("No active session", "system", None, false),
    }
}

async fn handle_usage_command(current_session_id: &str, state: &AppState) -> CommandResult {
    let session = {
        let sessions = state.sessions.lock().await;
        sessions.get(current_session_id).cloned()
    };
    match session {
        Some(session) => command_result(
            build_usage_report(&session, &gather_global_today_usage(state).await),
            "system",
            None,
            false,
        ),
        None => command_result("No active session", "system", None, false),
    }
}

async fn handle_clear_command(current_session_id: &str, state: &AppState) -> CommandResult {
    match reset_session_context_and_persist(state, current_session_id).await {
        Ok(()) => command_result_with_history(
            "Session cleared. System prompt preserved.",
            "system",
            None,
            true,
        ),
        Err(err) if err == "Session not found" => command_result(err, "system", None, false),
        Err(err) => command_result(
            format!("Failed to persist cleared session: {err}"),
            "error",
            None,
            false,
        ),
    }
}

fn handle_skills_command() -> CommandResult {
    let list = tools::tool_specs()
        .iter()
        .map(|spec| {
            let short = spec
                .description
                .split('.')
                .next()
                .unwrap_or(spec.description);
            format!("  {} → {}", spec.name, short)
        })
        .collect::<Vec<_>>()
        .join("\n");
    command_result(format!("Skills:\n{list}"), "system", None, false)
}

fn format_mcp_reports(reports: &[tools::mcp::McpServerLoadReport]) -> String {
    let mut lines = Vec::with_capacity(reports.len() * 2 + 1);
    lines.push("MCP servers:".to_string());

    for report in reports {
        match &report.error {
            Some(error) => {
                lines.push(format!(
                    "- {}: failed to load ({error})",
                    report.server_name
                ));
            }
            None if report.tool_names.is_empty() => {
                lines.push(format!("- {}: loaded 0 tools", report.server_name));
            }
            None => {
                lines.push(format!(
                    "- {}: loaded {} tools",
                    report.server_name,
                    report.tool_names.len()
                ));
                lines.push(format!("  tools: {}", report.tool_names.join(", ")));
            }
        }
    }

    lines.join("\n")
}

async fn handle_mcp_command(current_session_id: &str, state: &AppState) -> CommandResult {
    let workspace = {
        let sessions = state.sessions.lock().await;
        match sessions.get(current_session_id) {
            Some(session) => session.workspace.clone(),
            None => return command_result("No active session", "system", None, false),
        }
    };

    let enabled_servers = state
        .config
        .mcp_servers
        .values()
        .filter(|server| server.enabled)
        .count();
    if enabled_servers == 0 {
        return command_result("No MCP servers enabled.", "system", None, false);
    }

    let reports = tools::mcp::inspect_servers(&state.config, &workspace).await;
    command_result(format_mcp_reports(&reports), "system", None, false)
}

async fn handle_think_command(
    arg: &str,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    const VALID_LEVELS: &[&str] = &["auto", "off", "minimal", "low", "medium", "high", "xhigh"];

    if arg.is_empty() {
        let sessions = state.sessions.lock().await;
        let level = sessions
            .get(current_session_id)
            .map(|s| s.think_level.as_str())
            .unwrap_or("auto");
        return command_result(
            format!("think: {level}\nUsage: /think <auto|off|minimal|low|medium|high|xhigh>"),
            "system",
            None,
            false,
        );
    }

    let level = arg.to_lowercase();
    if !VALID_LEVELS.contains(&level.as_str()) {
        return command_result(
            format!(
                "Invalid think level: {arg}\nValid: auto, off, minimal, low, medium, high, xhigh"
            ),
            "system",
            None,
            false,
        );
    }

    match persist_session_update(
        state,
        current_session_id,
        |session| (session.think_level.clone(), session.updated_at),
        |session| {
            session.think_level = level.clone();
        },
        |session, (think_level, updated_at)| {
            session.think_level = think_level;
            session.updated_at = updated_at;
        },
    )
    .await
    {
        Ok(()) => command_result(format!("Think mode set to: {level}"), "system", None, true),
        Err(err) if err == "Session not found" => command_result(err, "system", None, false),
        Err(err) => command_result(
            format!("Failed to persist think level: {err}"),
            "error",
            None,
            false,
        ),
    }
}

async fn handle_react_command(
    arg: &str,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    if arg.is_empty() {
        let sessions = state.sessions.lock().await;
        let on = sessions
            .get(current_session_id)
            .map(|s| s.show_react)
            .unwrap_or_else(default_show_react);
        return command_result(
            format!(
                "react: {}\nUsage: /react <on|off>",
                if on { "on" } else { "off" }
            ),
            "system",
            None,
            false,
        );
    }

    let on = match parse_toggle_value(arg, "react") {
        Ok(value) => value,
        Err(err) => return command_result(err, "system", None, false),
    };
    match persist_session_update(
        state,
        current_session_id,
        |session| (session.show_react, session.updated_at),
        |session| {
            session.show_react = on;
        },
        |session, (show_react, updated_at)| {
            session.show_react = show_react;
            session.updated_at = updated_at;
        },
    )
    .await
    {
        Ok(()) => command_result(
            format!("React visibility: {}", if on { "on" } else { "off" }),
            "system",
            None,
            true,
        ),
        Err(err) if err == "Session not found" => command_result(err, "system", None, false),
        Err(err) => command_result(
            format!("Failed to persist react visibility: {err}"),
            "error",
            None,
            false,
        ),
    }
}

async fn handle_tool_command(
    arg: &str,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    if arg.is_empty() {
        let sessions = state.sessions.lock().await;
        let on = sessions
            .get(current_session_id)
            .map(|s| s.show_tools)
            .unwrap_or_else(default_show_tools);
        return command_result(
            format!(
                "tool: {}\nUsage: /tool <on|off>",
                if on { "on" } else { "off" }
            ),
            "system",
            None,
            false,
        );
    }

    let on = match parse_toggle_value(arg, "tool") {
        Ok(value) => value,
        Err(err) => return command_result(err, "system", None, false),
    };

    match persist_session_update(
        state,
        current_session_id,
        |session| (session.show_tools, session.updated_at),
        |session| {
            session.show_tools = on;
        },
        |session, (show_tools, updated_at)| {
            session.show_tools = show_tools;
            session.updated_at = updated_at;
        },
    )
    .await
    {
        Ok(()) => command_result_with_history(
            format!("Tool visibility: {}", if on { "on" } else { "off" }),
            "system",
            None,
            true,
        ),
        Err(err) if err == "Session not found" => command_result(err, "system", None, false),
        Err(err) => command_result(
            format!("Failed to persist tool visibility: {err}"),
            "error",
            None,
            false,
        ),
    }
}

async fn handle_reasoning_command(
    arg: &str,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    if arg.is_empty() {
        let sessions = state.sessions.lock().await;
        let on = sessions
            .get(current_session_id)
            .map(|s| s.show_reasoning)
            .unwrap_or_else(default_show_reasoning);
        return command_result(
            format!(
                "reasoning: {}\nUsage: /reasoning <on|off>",
                if on { "on" } else { "off" }
            ),
            "system",
            None,
            false,
        );
    }

    let on = match parse_toggle_value(arg, "reasoning") {
        Ok(value) => value,
        Err(err) => return command_result(err, "system", None, false),
    };

    match persist_session_update(
        state,
        current_session_id,
        |session| (session.show_reasoning, session.updated_at),
        |session| {
            session.show_reasoning = on;
        },
        |session, (show_reasoning, updated_at)| {
            session.show_reasoning = show_reasoning;
            session.updated_at = updated_at;
        },
    )
    .await
    {
        Ok(()) => command_result(
            format!("Reasoning visibility: {}", if on { "on" } else { "off" }),
            "system",
            None,
            true,
        ),
        Err(err) if err == "Session not found" => command_result(err, "system", None, false),
        Err(err) => command_result(
            format!("Failed to persist reasoning visibility: {err}"),
            "error",
            None,
            false,
        ),
    }
}

fn handle_help_command(current_session_id: &str) -> CommandResult {
    let mut help = "\
Commands:
    /new             Compress conversation to memory & clear context
    /status          Show session status
    /mcp             Show MCP load status
    /usage           Show session token usage
    /model [name]    Show or switch model
    /think [level]   Set thinking mode (auto|off|minimal|low|medium|high|xhigh)
    /react [on|off]  Toggle ReAct phase visibility
    /tool [on|off]   Toggle tool card visibility
    /reasoning [on|off] Toggle reasoning visibility
    /stop            Stop the running agent
    /skills          List available skills
    /rename <name>   Rename current session
    /clear           Clear messages (keep system prompt)
    /help            Show this help"
        .to_string();
    if current_session_id == MAIN_SESSION_ID {
        help.push_str(
            "\n\nMain session commands:\n\
        /sessions        List all active sessions\n\
        /delete <id>     Delete a session by full ID or unique prefix",
        );
    }
    command_result(help, "system", None, false)
}

async fn handle_sessions_command(current_session_id: &str, state: &AppState) -> CommandResult {
    if current_session_id != MAIN_SESSION_ID {
        return command_result(
            "This command is only available in the main session.",
            "error",
            None,
            false,
        );
    }
    command_result(gather_sessions_status(state).await, "system", None, false)
}

async fn handle_delete_command(
    arg: &str,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    if current_session_id != MAIN_SESSION_ID {
        return command_result(
            "This command is only available in the main session.",
            "error",
            None,
            false,
        );
    }
    if arg.is_empty() {
        return command_result("Usage: /delete <session_id>", "system", None, false);
    }
    let result = delete_session_by_id(arg, state).await;
    let changed = result.starts_with("Deleted");
    command_result(result, "system", None, changed)
}

pub(crate) async fn handle_command(
    input: &str,
    current_session_id: &str,
    connection_id: u64,
    state: &AppState,
    tx: &WsTx,
    cancel: &CancellationToken,
) -> Option<CommandResult> {
    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd {
        "/new" => handle_new_command(current_session_id, state, tx, cancel).await,
        "/session_new" => Some(handle_session_new_command(current_session_id, state).await),
        "/switch" => {
            Some(handle_switch_command(arg, current_session_id, connection_id, state).await)
        }

        "/rename" => {
            if arg.is_empty() {
                return Some(command_result(
                    "Usage: /rename <new_name>",
                    "system",
                    None,
                    false,
                ));
            }
            let name = arg.to_string();
            match persist_session_update(
                state,
                current_session_id,
                |session| (session.name.clone(), session.updated_at),
                |session| {
                    session.name = name;
                },
                |session, (session_name, updated_at)| {
                    session.name = session_name;
                    session.updated_at = updated_at;
                },
            )
            .await
            {
                Ok(()) => Some(command_result(
                    format!("Renamed to: {arg}"),
                    "system",
                    None,
                    true,
                )),
                Err(err) if err == "Session not found" => {
                    Some(command_result(err, "system", None, false))
                }
                Err(err) => Some(command_result(
                    format!("Failed to persist rename: {err}"),
                    "error",
                    None,
                    false,
                )),
            }
        }

        "/model" => Some(handle_model_command(arg, current_session_id, state).await),
        "/status" => Some(handle_status_command(current_session_id, state).await),
        "/mcp" => Some(handle_mcp_command(current_session_id, state).await),
        "/usage" => Some(handle_usage_command(current_session_id, state).await),
        "/clear" => Some(handle_clear_command(current_session_id, state).await),
        "/skills" => Some(handle_skills_command()),
        "/think" => Some(handle_think_command(arg, current_session_id, state).await),
        "/react" => Some(handle_react_command(arg, current_session_id, state).await),
        "/tool" => Some(handle_tool_command(arg, current_session_id, state).await),
        "/reasoning" => Some(handle_reasoning_command(arg, current_session_id, state).await),
        "/help" => Some(handle_help_command(current_session_id)),
        "/sessions" => Some(handle_sessions_command(current_session_id, state).await),
        "/delete" => Some(handle_delete_command(arg, current_session_id, state).await),

        // /stop when not busy — the in-flight case is handled by the agent loop drain
        "/stop" => Some(command_result("No active run to stop.", "system", None, false)),

        _ => None,
    }
}

#[cfg(test)]
#[path = "tests/commands_tests.rs"]
mod tests;
