use std::collections::HashSet;
use std::path::Path;

use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::{
    AppState, ChatMessage, MAIN_SESSION_ID, Session, WsTx, agent, build_system_prompt,
    build_system_prompt_with_query, default_show_react, default_show_reasoning, default_show_tools,
    memory, now_epoch, prompts, providers,
    session_admin::gather_global_today_usage,
    session_store::{
        build_session_status, build_usage_report, replace_session_messages, save_session_to_disk,
    },
    tools, truncate, ws_send,
};

// ── Chat Commands ────────────────────────────────────────────────────────────

pub(crate) struct CommandResult {
    pub(crate) response: String,
    pub(crate) response_type: &'static str,
    pub(crate) sessions_changed: bool,
    pub(crate) refresh_history: bool,
    pub(crate) dismissible: bool,
}

pub(crate) fn command_result(
    response: impl Into<String>,
    response_type: &'static str,
    sessions_changed: bool,
) -> CommandResult {
    CommandResult {
        response: response.into(),
        response_type,
        sessions_changed,
        refresh_history: false,
        dismissible: true,
    }
}

pub(crate) fn command_result_with_history(
    response: impl Into<String>,
    response_type: &'static str,
    sessions_changed: bool,
) -> CommandResult {
    CommandResult {
        refresh_history: true,
        ..command_result(response, response_type, sessions_changed)
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

async fn status_effective_think_level(
    session: &Session,
    state: &AppState,
    resolved: &providers::ResolvedModel,
) -> String {
    if session.think_level != "auto" {
        return session.think_level.clone();
    }
    if !providers::auto_think_supported(resolved) {
        return "off".to_string();
    }

    let live_round = { state.live_rounds.lock().await.get(&session.id).cloned() };
    let cycles = live_round
        .as_ref()
        .and_then(|round| round.cycle)
        .unwrap_or(0);
    let has_observation = live_round
        .as_ref()
        .map(|round| round.has_observation)
        .unwrap_or(false);
    let user_msg_chars = session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .and_then(|m| m.content.as_ref())
        .map(|c| c.chars().count())
        .unwrap_or(0);
    agent::auto_think_level(cycles, has_observation, user_msg_chars, 0).to_string()
}

async fn build_runtime_status(session: &Session, state: &AppState) -> String {
    let config = state.config();
    let model = session.effective_model(&config.model).to_string();
    let resolved = config.resolve_model(&model);
    let effective_think = status_effective_think_level(session, state, &resolved).await;
    let mut extra_tools = Vec::new();
    let mut cached_mcp_tools = match resolved.provider {
        crate::Provider::Anthropic => {
            tools::mcp::cached_tool_definitions_anthropic(&config, &session.workspace)
        }
        crate::Provider::OpenAI => {
            tools::mcp::cached_tool_definitions_openai(&config, &session.workspace)
        }
        crate::Provider::Ollama => {
            tools::mcp::cached_tool_definitions_ollama(&config, &session.workspace)
        }
        crate::Provider::Gemini => {
            tools::mcp::cached_tool_definitions_gemini(&config, &session.workspace)
        }
    };
    extra_tools.append(&mut cached_mcp_tools);
    let (cached_mcp_servers, enabled_mcp_servers) =
        tools::mcp::cached_server_counts(&config, &session.workspace);
    let request_budget =
        crate::context::context_input_budget_for_runtime(&config, &model, &effective_think);
    let tool_estimate =
        crate::context::estimate_tool_schema_tokens_for_provider(resolved.provider, &extra_tools);

    let mut request_messages = session.messages.clone();
    let fresh_system = build_system_prompt(
        &config,
        &session.workspace,
        &model,
        &session.disabled_system_skills,
    );
    if let Some(first) = request_messages.first_mut()
        && first.role == "system"
    {
        *first = fresh_system;
    }

    let request_estimate = crate::context::estimate_request_tokens_for_provider(
        resolved.provider,
        &request_messages,
        &extra_tools,
    );

    let mcp_cache_line = if enabled_mcp_servers > 0 {
        format!(
            "\nmcp_schema_cache: {}/{} enabled server(s) cached",
            cached_mcp_servers, enabled_mcp_servers
        )
    } else {
        String::new()
    };
    let request_note = if enabled_mcp_servers > cached_mcp_servers {
        format!(
            "includes refreshed system prompt, built-in tool schemas, cached runtime tool schemas, and runtime reasoning reserve; uncached MCP servers are excluded from this estimate ({cached_mcp_servers}/{enabled_mcp_servers} cached)"
        )
    } else {
        "includes refreshed system prompt, built-in/runtime tool schemas, and runtime reasoning reserve".to_string()
    };

    format!(
        "{}\nrequest_est: {}/{} (tools {} think {})\nrequest_status: {}{}\nrequest_note: {}",
        build_session_status(session, &config),
        crate::format_token_count(request_estimate as u64),
        crate::format_token_count(request_budget as u64),
        crate::format_token_count(tool_estimate as u64),
        effective_think,
        if request_estimate > request_budget {
            "over budget"
        } else {
            "ok"
        },
        mcp_cache_line,
        request_note,
    )
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
    let config = state.config();
    persist_session_update(
        state,
        current_session_id,
        |session| {
            (
                session.messages.clone(),
                session.subagent_snapshots.clone(),
                session.failed_tool_results.clone(),
                session.tool_calls_count,
                session.updated_at,
            )
        },
        |session| {
            let model = session.effective_model(&config.model).to_string();
            let sys = build_system_prompt(
                &config,
                &session.workspace,
                &model,
                &session.disabled_system_skills,
            );
            replace_session_messages(session, vec![sys]);
            session.tool_calls_count = 0;
        },
        |session, (messages, subagent_snapshots, failed_tool_results, tool_calls_count, updated_at)| {
            session.messages = messages;
            session.subagent_snapshots = subagent_snapshots;
            session.failed_tool_results = failed_tool_results;
            session.tool_calls_count = tool_calls_count;
            session.updated_at = updated_at;
        },
    )
    .await
}

async fn handle_new_command(
    current_session_id: &str,
    state: &AppState,
    tx: &WsTx,
    cancel: &CancellationToken,
) -> Option<CommandResult> {
    let config = state.config();
    let (conversation_text, workspace, model_str) = {
        let sessions = state.sessions.lock().await;
        let session = match sessions.get(current_session_id) {
            Some(s) => s,
            None => return Some(command_result("Session not found", "system", false)),
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
                    if let Some(c) = &msg.content
                        && !c.is_empty()
                    {
                        lines.push(format!("Assistant: {c}"));
                    }
                }
                _ => {}
            }
        }
        (
            lines.join("\n"),
            session.workspace.clone(),
            session.effective_model(&config.model).to_string(),
        )
    };

    if conversation_text.is_empty() {
        match reset_session_context_and_persist(state, current_session_id).await {
            Ok(()) => {
                return Some(command_result_with_history(
                    "Context cleared.",
                    "system",
                    true,
                ));
            }
            Err(err) if err == "Session not found" => {
                return Some(command_result(err, "system", false));
            }
            Err(err) => {
                return Some(command_result(
                    format!("Failed to persist cleared context: {err}"),
                    "error",
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
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: None,
        },
        ChatMessage {
            role: "user".into(),
            content: Some(truncate(&conversation_text, 60_000)),
            images: None,
            thinking: None,
            anthropic_thinking_blocks: None,
            tool_calls: None,
            tool_call_id: None,
            timestamp: Some(now_epoch()),
        },
    ];
    let resolved = config.resolve_model(&model_str);
    let summary = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            return Some(command_result(
                "Shutdown: compression skipped, context unchanged.",
                "system",
                false,
            ));
        }
        result = providers::call_llm_simple_with_usage(&state.http, &resolved, &compress_prompt, &workspace, config.s3.as_ref(), config.max_llm_retries) => {
            match result {
                Ok(s) => s,
                Err(e) => {
                    return Some(command_result(
                        format!("Failed to compress conversation: {e}"),
                        "system",
                        false,
                    ));
                }
            }
        }
    };

    let provider_name = config.resolve_provider_name(&model_str);
    let input_tokens = summary.input_tokens.unwrap_or_else(|| {
        crate::estimate_tokens_for_provider(resolved.provider, &compress_prompt) as u64
    });
    let output_tokens = summary.output_tokens.unwrap_or_else(|| {
        crate::message_token_len_for_provider(
            resolved.provider,
            &crate::ChatMessage {
                role: "assistant".into(),
                content: Some(summary.content.clone()),
                images: None,
                thinking: None,
                anthropic_thinking_blocks: None,
                tool_calls: None,
                tool_call_id: None,
                timestamp: None,
            },
        ) as u64
    });
    {
        let mut sessions = state.sessions.lock().await;
        if let Some(session) = sessions.get_mut(current_session_id) {
            crate::update_session_token_usage_with_provider(
                session,
                input_tokens,
                output_tokens,
                if summary.input_tokens.is_some() {
                    "provider"
                } else {
                    "estimated"
                },
                if summary.output_tokens.is_some() {
                    "provider"
                } else {
                    "estimated"
                },
                Some(&provider_name),
                Some(crate::context::USAGE_ROLE_CONTEXT),
            );
        }
    }
    let summary = summary.content;

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
            false,
        ));
    }

    match reset_session_context_and_persist(state, current_session_id).await {
        Ok(()) => {}
        Err(err) if err == "Session not found" => {
            return Some(command_result(err, "system", false));
        }
        Err(err) => {
            return Some(command_result(
                format!("Memory saved to memory/{today}.md but failed to clear context: {err}"),
                "error",
                false,
            ));
        }
    }

    Some(command_result_with_history(
        format!("Conversation compressed and saved to memory/{today}.md. Context cleared."),
        "success",
        true,
    ))
}

async fn handle_switch_command(
    _arg: &str,
    _current_session_id: &str,
    _connection_id: u64,
    _state: &AppState,
) -> CommandResult {
    command_result(
        "Single-session mode is enabled. LingClaw only keeps the main session.",
        "system",
        false,
    )
}

async fn handle_model_command(
    arg: &str,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    let config = state.config();
    if arg.is_empty() {
        let sessions = state.sessions.lock().await;
        let model = sessions
            .get(current_session_id)
            .map(|s| s.effective_model(&config.model))
            .unwrap_or(&config.model)
            .to_string();
        let current = config.canonical_model_ref(&model).unwrap_or(model.clone());
        let available = config.available_models();
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
            false,
        );
    }

    let canonical = match config.canonical_model_ref(arg) {
        Ok(value) => value,
        Err(err) => return command_result(err, "error", false),
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
        Ok(()) => command_result(format!("Model switched to: {canonical}"), "system", true),
        Err(err) if err == "Session not found" => command_result(err, "system", false),
        Err(err) => command_result(
            format!("Failed to persist model switch: {err}"),
            "error",
            false,
        ),
    }
}

async fn handle_status_command(current_session_id: &str, state: &AppState) -> CommandResult {
    let session = {
        let sessions = state.sessions.lock().await;
        sessions.get(current_session_id).cloned()
    };
    match session {
        Some(session) => {
            command_result(build_runtime_status(&session, state).await, "system", false)
        }
        None => command_result("No active session", "system", false),
    }
}

async fn handle_system_prompt_command(current_session_id: &str, state: &AppState) -> CommandResult {
    let config = state.config();
    let session = {
        let sessions = state.sessions.lock().await;
        sessions.get(current_session_id).cloned()
    };

    match session {
        Some(session) => {
            let model = session.effective_model(&config.model).to_string();
            let resolved = config.resolve_model(&model);
            let latest_query = session
                .messages
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .and_then(|message| message.content.as_deref());
            let system_prompt = build_system_prompt_with_query(
                &config,
                &session.workspace,
                &model,
                &session.disabled_system_skills,
                latest_query,
            );
            let prompt_tokens =
                crate::context::message_token_len_for_provider(resolved.provider, &system_prompt);
            let prompt_text = system_prompt.content.unwrap_or_default();

            command_result(
                format!(
                    "Current system prompt\nmodel: {}\nprovider: {}\nestimated_tokens: {} ({})\nnote: base system prompt only; excludes per-cycle observation hints and planning/finish nudges.\n{}\n{}",
                    model,
                    resolved.provider.label(),
                    crate::format_token_count(prompt_tokens as u64),
                    prompt_tokens,
                    "─".repeat(40),
                    prompt_text,
                ),
                "system",
                false,
            )
        }
        None => command_result("No active session", "system", false),
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
            false,
        ),
        None => command_result("No active session", "system", false),
    }
}

async fn handle_clear_command(current_session_id: &str, state: &AppState) -> CommandResult {
    match reset_session_context_and_persist(state, current_session_id).await {
        Ok(()) => {
            command_result_with_history("Session cleared. System prompt preserved.", "system", true)
        }
        Err(err) if err == "Session not found" => command_result(err, "system", false),
        Err(err) => command_result(
            format!("Failed to persist cleared session: {err}"),
            "error",
            false,
        ),
    }
}

async fn handle_skills_command(
    filter: Option<prompts::SkillSource>,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    let workspace = state
        .sessions
        .lock()
        .await
        .get(current_session_id)
        .map(|s| s.workspace.clone());

    let ws = workspace.as_deref().unwrap_or(Path::new(""));

    let skills = match filter {
        Some(source) => prompts::discover_skills_by_source(ws, source),
        None => prompts::discover_all_skills(ws),
    };

    let label = match filter {
        Some(prompts::SkillSource::System) => "System skills",
        Some(prompts::SkillSource::Global) => "Global skills",
        Some(prompts::SkillSource::Session) => "Session skills",
        None => "All skills",
    };

    let mut output = if filter.is_none() {
        let tool_list = tools::tool_specs()
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
        format!("Tools:\n{tool_list}\n\n{label}:")
    } else {
        format!("{label}:")
    };

    if skills.is_empty() {
        output.push_str("\n  (none)");
    } else {
        for skill in &skills {
            let source_tag = if filter.is_none() {
                format!(" [{}]", skill.source.label())
            } else {
                String::new()
            };
            if skill.description.is_empty() {
                output.push_str(&format!(
                    "\n  {}{} ({})",
                    skill.name, source_tag, skill.path
                ));
            } else {
                output.push_str(&format!(
                    "\n  {}{} → {} ({})",
                    skill.name, source_tag, skill.description, skill.path
                ));
            }
        }
    }

    command_result(output, "system", false)
}

/// Handle `/skills-system [install|uninstall <pattern>]`.
///
/// Without arguments: list all system skills with loaded/disabled status.
/// `uninstall <pattern>`: disable system skills matching the pattern (e.g. `anthropics`, `anthropics/pdf`).
/// `install <pattern>`:   re-enable previously disabled system skills.
async fn handle_skills_system_command(
    arg: &str,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    let parts: Vec<&str> = arg.splitn(2, ' ').collect();
    let sub = parts.first().map(|s| s.trim()).unwrap_or("");

    match sub {
        "" => show_system_skills_status(current_session_id, state).await,
        "uninstall" | "disable" => {
            let pattern = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if pattern.is_empty() {
                return command_result(
                    "Usage: /skills-system uninstall <pattern>\n\
                     Examples:\n\
                     \x20 /skills-system uninstall anthropics        — uninstall all anthropics skills\n\
                     \x20 /skills-system uninstall anthropics/pdf    — uninstall only the pdf skill",
                    "system",
                    false,
                );
            }
            toggle_system_skill(current_session_id, state, pattern, true).await
        }
        "install" | "enable" => {
            let pattern = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if pattern.is_empty() {
                return command_result(
                    "Usage: /skills-system install <pattern>\n\
                     Examples:\n\
                     \x20 /skills-system install anthropics        — re-install all anthropics skills\n\
                     \x20 /skills-system install anthropics/pdf    — re-install only the pdf skill",
                    "system",
                    false,
                );
            }
            toggle_system_skill(current_session_id, state, pattern, false).await
        }
        _ => command_result(
            "Unknown subcommand. Usage:\n\
             \x20 /skills-system                         — show system skills status\n\
             \x20 /skills-system uninstall <pattern>     — disable a skill or group\n\
             \x20 /skills-system install <pattern>       — re-enable a skill or group",
            "system",
            false,
        ),
    }
}

async fn show_system_skills_status(current_session_id: &str, state: &AppState) -> CommandResult {
    let (workspace, disabled) = {
        let sessions = state.sessions.lock().await;
        let Some(session) = sessions.get(current_session_id) else {
            return command_result("Session not found.", "error", false);
        };
        (
            session.workspace.clone(),
            session.disabled_system_skills.clone(),
        )
    };

    let skills = prompts::discover_skills_by_source(&workspace, prompts::SkillSource::System);

    let mut output = String::from("System skills:");
    if skills.is_empty() {
        output.push_str("\n  (none)");
        // Diagnostic: show resolved path to help troubleshoot
        match prompts::system_skills_resolved_path() {
            Some(p) => output.push_str(&format!("\n  Search resolved to: {}", p.display())),
            None => {
                output.push_str("\n  No system skills directory found.");
                output.push_str(
                    "\n  Expected: ~/.lingclaw/system-skills/ or docs/reference/skills/ near the binary.",
                );
                output.push_str(
                    "\n  Run `lingclaw install` from the source directory to deploy system skills.",
                );
            }
        }
    } else {
        for skill in &skills {
            let is_disabled = prompts::is_system_skill_disabled(&skill.path, &disabled);
            let status = if is_disabled { "disabled" } else { "loaded" };
            let status_icon = if is_disabled { "✗" } else { "✓" };
            if skill.description.is_empty() {
                output.push_str(&format!(
                    "\n  {status_icon} [{status}] {} ({})",
                    skill.name, skill.path
                ));
            } else {
                output.push_str(&format!(
                    "\n  {status_icon} [{status}] {} → {} ({})",
                    skill.name, skill.description, skill.path
                ));
            }
        }
    }

    if !disabled.is_empty() {
        let mut sorted: Vec<_> = disabled.iter().cloned().collect();
        sorted.sort();
        output.push_str(&format!("\n\nDisabled patterns: {}", sorted.join(", ")));
    }

    command_result(output, "system", false)
}

/// Extract the relative directory from a system skill path.
/// `system://skills/anthropics/pdf/SKILL.md` → `anthropics/pdf`
fn skill_relative_dir(path: &str) -> String {
    const PREFIX: &str = "system://skills/";
    let relative = path.strip_prefix(PREFIX).unwrap_or(path);
    relative
        .strip_suffix("/SKILL.md")
        .unwrap_or(relative)
        .to_string()
}

async fn toggle_system_skill(
    current_session_id: &str,
    state: &AppState,
    pattern: &str,
    disable: bool,
) -> CommandResult {
    // Normalise pattern: strip leading/trailing slashes
    let pattern = pattern.trim_matches('/').to_string();

    // Validate the pattern matches at least one discovered system skill
    let workspace = {
        let sessions = state.sessions.lock().await;
        sessions
            .get(current_session_id)
            .map(|s| s.workspace.clone())
    };
    let ws = workspace.as_deref().unwrap_or(std::path::Path::new(""));
    let system_skills = prompts::discover_skills_by_source(ws, prompts::SkillSource::System);
    let matched: Vec<_> = system_skills
        .iter()
        .filter(|s| prompts::is_system_skill_disabled(&s.path, &HashSet::from([pattern.clone()])))
        .collect();

    if matched.is_empty() {
        let groups = prompts::list_system_skill_groups();
        let hint = if groups.is_empty() {
            String::new()
        } else {
            format!("\nAvailable groups: {}", groups.join(", "))
        };
        return command_result(
            format!("No system skills match pattern: {pattern}{hint}"),
            "error",
            false,
        );
    }

    // Pre-compute the new disabled set outside the closure so we have access to
    // `system_skills` for the parent-pattern expansion logic (install sub-skill
    // when a parent group is disabled → replace parent with sibling disables).
    let compute_new_disabled = |current: &HashSet<String>| -> HashSet<String> {
        let mut new_set = current.clone();
        if disable {
            new_set.insert(pattern.clone());
        } else {
            // Remove exact match and any sub-patterns covered by this install
            new_set.retain(|p| p != &pattern && !p.starts_with(&format!("{}/", pattern)));

            // If a parent pattern still covers the installed pattern, expand it:
            // e.g. disabled={"anthropics"}, install "anthropics/pdf" →
            //   remove "anthropics", add individual disables for all siblings.
            let parents: Vec<String> = new_set
                .iter()
                .filter(|p| pattern.starts_with(&format!("{}/", p)))
                .cloned()
                .collect();
            for parent in parents {
                new_set.remove(&parent);
                // Add individual disable entries for sibling skills not being installed
                for skill in &system_skills {
                    let rel = skill_relative_dir(&skill.path);
                    if prompts::is_system_skill_disabled(
                        &skill.path,
                        &HashSet::from([parent.clone()]),
                    ) && !prompts::is_system_skill_disabled(
                        &skill.path,
                        &HashSet::from([pattern.clone()]),
                    ) {
                        new_set.insert(rel);
                    }
                }
            }
        }
        new_set
    };

    let pattern_for_msg = pattern.clone();
    match persist_session_update(
        state,
        current_session_id,
        |session| session.disabled_system_skills.clone(),
        |session| {
            session.disabled_system_skills = compute_new_disabled(&session.disabled_system_skills);
        },
        |session, old| {
            session.disabled_system_skills = old;
        },
    )
    .await
    {
        Ok(()) => {
            crate::prompts::invalidate_skills_cache();
            let verb = if disable { "Disabled" } else { "Enabled" };
            let names: Vec<_> = matched.iter().map(|s| s.name.as_str()).collect();
            command_result(
                format!(
                    "{verb} {} skill(s) matching \"{pattern_for_msg}\": {}",
                    matched.len(),
                    names.join(", ")
                ),
                "system",
                true,
            )
        }
        Err(err) => command_result(format!("Failed to persist change: {err}"), "error", false),
    }
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

async fn handle_mcp_command_with_arg(
    arg: &str,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    let config = state.config();
    let workspace = {
        let sessions = state.sessions.lock().await;
        match sessions.get(current_session_id) {
            Some(session) => session.workspace.clone(),
            None => return command_result("No active session", "system", false),
        }
    };

    let enabled_servers = config
        .mcp_servers
        .values()
        .filter(|server| server.enabled)
        .count();
    if enabled_servers == 0 {
        return command_result("No MCP servers enabled.", "system", false);
    }

    match arg {
        "" => {
            let reports = tools::mcp::inspect_servers(&config, &workspace).await;
            command_result(format_mcp_reports(&reports), "system", false)
        }
        "refresh" => match tools::mcp::refresh_servers(&config, &workspace).await {
            Ok(reports) => command_result(
                format!("Refreshed MCP cache.\n\n{}", format_mcp_reports(&reports)),
                "system",
                false,
            ),
            Err(error) => command_result(
                format!("Failed to refresh MCP cache: {error}"),
                "error",
                false,
            ),
        },
        _ => command_result("Usage: /mcp [refresh]", "system", false),
    }
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
        Ok(()) => command_result(format!("Think mode set to: {level}"), "system", true),
        Err(err) if err == "Session not found" => command_result(err, "system", false),
        Err(err) => command_result(
            format!("Failed to persist think level: {err}"),
            "error",
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
            false,
        );
    }

    let on = match parse_toggle_value(arg, "react") {
        Ok(value) => value,
        Err(err) => return command_result(err, "system", false),
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
            true,
        ),
        Err(err) if err == "Session not found" => command_result(err, "system", false),
        Err(err) => command_result(
            format!("Failed to persist react visibility: {err}"),
            "error",
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
            false,
        );
    }

    let on = match parse_toggle_value(arg, "tool") {
        Ok(value) => value,
        Err(err) => return command_result(err, "system", false),
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
            true,
        ),
        Err(err) if err == "Session not found" => command_result(err, "system", false),
        Err(err) => command_result(
            format!("Failed to persist tool visibility: {err}"),
            "error",
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
            false,
        );
    }

    let on = match parse_toggle_value(arg, "reasoning") {
        Ok(value) => value,
        Err(err) => return command_result(err, "system", false),
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
            true,
        ),
        Err(err) if err == "Session not found" => command_result(err, "system", false),
        Err(err) => command_result(
            format!("Failed to persist reasoning visibility: {err}"),
            "error",
            false,
        ),
    }
}

fn handle_help_command(current_session_id: &str) -> CommandResult {
    let mut help = "\
Commands:
    /new             Compress conversation to memory & clear context
    /status          Show session status
    /system-prompt   Show current system prompt and estimated tokens
    /mcp [refresh]   Show MCP load status or refresh cache
    /usage           Show session token usage
    /model [name]    Show or switch model
    /think [level]   Set thinking mode (auto|off|minimal|low|medium|high|xhigh)
    /react [on|off]  Toggle ReAct phase visibility
    /tool [on|off]   Toggle tool card visibility
    /reasoning [on|off] Toggle reasoning visibility
    /stop            Stop the running agent
    /skills          List available tools and skills
    /skills-system   List system skills with status (install/uninstall subcommands)
    /skills-global   List global skills (~/.lingclaw/skills/)
    /skills-session  List session-local skills
    /agents          List discovered sub-agents
    /clear           Clear messages (keep system prompt)
    /memory [stats|debug] Show structured memory status or updater diagnostics
    /reflection [today|yesterday|list] Show daily reflection status and reflection entries
    /help            Show this help"
        .to_string();
    if current_session_id == MAIN_SESSION_ID {
        help.push_str("\n\nSingle-session mode: LingClaw keeps only the main session.");
    }
    command_result(help, "system", false)
}

async fn handle_sessions_command(_current_session_id: &str, _state: &AppState) -> CommandResult {
    command_result(
        "Single-session mode is enabled. Only the main session is available.",
        "system",
        false,
    )
}

async fn handle_delete_command(
    _arg: &str,
    _current_session_id: &str,
    _state: &AppState,
) -> CommandResult {
    command_result(
        "Single-session mode is enabled. The main session cannot be deleted.",
        "system",
        false,
    )
}

async fn handle_memory_command(
    arg: &str,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    let config = state.config();
    if !config.structured_memory {
        return command_result(
            "Structured memory is disabled. Enable with `\"structuredMemory\": true` in settings or `LINGCLAW_STRUCTURED_MEMORY=true`.",
            "system",
            false,
        );
    }
    let workspace = {
        let sessions = state.sessions.lock().await;
        match sessions.get(current_session_id) {
            Some(s) => s.workspace.clone(),
            None => return command_result("Session not found", "error", false),
        }
    };

    let memory_queue = state.memory_queue();
    let response = match arg {
        "" => format!(
            "{}\n\n{}",
            memory::memory_status(&workspace),
            memory::memory_runtime_status(memory_queue.as_ref())
        ),
        "stats" => memory::memory_runtime_status(memory_queue.as_ref()),
        "debug" => format!(
            "{}\n\n{}",
            memory::memory_status(&workspace),
            memory::memory_debug_status(&workspace, memory_queue.as_ref())
        ),
        _ => return command_result("Usage: /memory [stats|debug]", "system", false),
    };

    command_result(response, "system", false)
}

async fn handle_reflection_command(
    arg: &str,
    current_session_id: &str,
    state: &AppState,
) -> CommandResult {
    let config = state.config();
    let workspace = {
        let sessions = state.sessions.lock().await;
        match sessions.get(current_session_id) {
            Some(s) => s.workspace.clone(),
            None => return command_result("Session not found", "error", false),
        }
    };
    let memory_dir = workspace.join("memory");
    let local = prompts::current_local_snapshot();
    let today = local.today();
    let yesterday = local.yesterday();
    let enabled = crate::runtime_loop::reflection_runtime_enabled();

    let response = match arg {
        "" => {
            // Overview: config + runtime status + today preview.
            let mut lines = vec![format!(
                "Daily Reflection: {}",
                if enabled {
                    "**enabled**"
                } else {
                    "**disabled**"
                }
            )];
            if enabled {
                let runtime = crate::runtime_loop::reflection_runtime_status();
                let model = config
                    .reflection_model
                    .as_deref()
                    .unwrap_or("(inherits memory_model or primary)");
                lines.push(format!("Model: {model}"));
                lines.push("Min cycles: 3 | Cooldown: 10 min".to_string());
                lines.push(runtime);
            } else {
                lines.push(
                    "Enable with `\"dailyReflection\": true` in settings or `LINGCLAW_DAILY_REFLECTION=true`."
                        .to_string(),
                );
            }
            let today_preview = read_reflection_entries_preview(&memory_dir, &today, 500).await;
            if let Some(preview) = today_preview {
                lines.push(format!(
                    "\n--- Today's reflections ({today}) ---\n{preview}"
                ));
            } else {
                lines.push(format!("\nNo reflections today ({today})."));
            }
            lines.join("\n")
        }
        "today" => read_reflection_entries_full(&memory_dir, &today)
            .await
            .unwrap_or_else(|| format!("No reflections for {today}.")),
        "yesterday" => read_reflection_entries_full(&memory_dir, &yesterday)
            .await
            .unwrap_or_else(|| format!("No reflections for {yesterday}.")),
        "list" => list_daily_memory_files(&memory_dir).await,
        _ => return command_result("Usage: /reflection [today|yesterday|list]", "system", false),
    };

    command_result(response, "system", false)
}

/// Extract only reflection sections from daily memory content.
/// Entries are recognized by their `## HH:MM Local` header shape, which avoids
/// mis-parsing Markdown horizontal rules inside the reflection body.
fn is_daily_memory_entry_header(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("## ") else {
        return false;
    };
    let bytes = rest.as_bytes();
    if bytes.len() < 11 {
        return false;
    }
    if !(bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b':'
        && bytes[3].is_ascii_digit()
        && bytes[4].is_ascii_digit()
        && &bytes[5..11] == b" Local")
    {
        return false;
    }
    bytes.len() == 11 || rest[11..].starts_with(" \u{2014} Reflection")
}

fn filter_reflection_sections(content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let entry_starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| is_daily_memory_entry_header(line).then_some(idx))
        .collect();
    let mut sections: Vec<String> = Vec::new();
    for (idx, start) in entry_starts.iter().copied().enumerate() {
        if !lines[start].contains("\u{2014} Reflection") {
            continue;
        }
        let mut end = entry_starts.get(idx + 1).copied().unwrap_or(lines.len());
        if idx + 1 < entry_starts.len() {
            while end > start {
                let line = lines[end - 1].trim();
                if line.is_empty() || line == "---" {
                    end -= 1;
                } else {
                    break;
                }
            }
        }
        let section = lines[start..end].join("\n");
        let trimmed = section.trim();
        if !trimmed.is_empty() {
            sections.push(trimmed.to_string());
        }
    }
    if sections.is_empty() {
        return None;
    }
    let joined = sections.join("\n\n---\n\n");
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Read reflection entries from a daily memory file, returning a truncated preview.
async fn read_reflection_entries_preview(
    memory_dir: &Path,
    date: &str,
    max_chars: usize,
) -> Option<String> {
    let path = memory_dir.join(format!("{date}.md"));
    let content = tokio::fs::read_to_string(path).await.ok()?;
    let filtered = filter_reflection_sections(&content)?;
    Some(truncate(&filtered, max_chars).to_string())
}

/// Read the full reflection entries from a daily memory file.
async fn read_reflection_entries_full(memory_dir: &Path, date: &str) -> Option<String> {
    let path = memory_dir.join(format!("{date}.md"));
    let content = tokio::fs::read_to_string(path).await.ok()?;
    filter_reflection_sections(&content)
}

/// List all daily memory files in the memory directory, newest first.
async fn list_daily_memory_files(memory_dir: &Path) -> String {
    let mut rd = match tokio::fs::read_dir(memory_dir).await {
        Ok(rd) => rd,
        Err(_) => return "No reflection files found.".to_string(),
    };
    let mut files: Vec<String> = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") {
            continue;
        }
        let Ok(content) = tokio::fs::read_to_string(entry.path()).await else {
            continue;
        };
        if filter_reflection_sections(&content).is_some() {
            files.push(name);
        }
    }
    if files.is_empty() {
        return "No reflection files found.".to_string();
    }
    files.sort();
    files.reverse();
    let mut lines = vec![format!("Reflection files ({} total):", files.len())];
    for f in &files {
        lines.push(format!("  {f}"));
    }
    lines.join("\n")
}

async fn handle_agents_command(current_session_id: &str, state: &AppState) -> CommandResult {
    let config = state.config();
    let workspace = {
        let sessions = state.sessions.lock().await;
        match sessions.get(current_session_id) {
            Some(s) => s.workspace.clone(),
            None => return command_result("Session not found", "error", false),
        }
    };

    // Ensure MCP tool cache is warm so the tool listing includes MCP tools.
    crate::tools::mcp::ensure_tools_cached(&config, &workspace).await;

    let agents = crate::subagents::discovery::discover_all_agents(&workspace);
    if agents.is_empty() {
        return command_result(
            "No sub-agents found.\n\n\
             Place AGENT.md files in:\n\
             - `docs/reference/agents/<name>/AGENT.md` (system)\n\
             - `~/.lingclaw/agents/<name>/AGENT.md` (global)\n\
             - `agents/<name>/AGENT.md` (session workspace)",
            "system",
            false,
        );
    }

    let mut lines = Vec::with_capacity(agents.len() + 2);
    lines.push(format!("**{} sub-agent(s) available:**\n", agents.len()));
    for agent in &agents {
        let tools = crate::subagents::filter_tools_for_agent_with_mcp(agent, &config, &workspace);
        let tool_list = if tools.is_empty() {
            "(no tools)".to_string()
        } else {
            tools.join(", ")
        };
        let model_info = config.sub_agent_model_for(&agent.name);
        lines.push(format!(
            "- **{}** [`{}`] — {}\n  model: {} | max_turns: {} | tools: {}",
            agent.name,
            agent.source.label(),
            if agent.description.is_empty() {
                "(no description)"
            } else {
                &agent.description
            },
            model_info,
            agent.max_turns,
            tool_list,
        ));
    }

    command_result(lines.join("\n"), "system", false)
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
        "/switch" => {
            Some(handle_switch_command(arg, current_session_id, connection_id, state).await)
        }

        "/model" => Some(handle_model_command(arg, current_session_id, state).await),
        "/status" => Some(handle_status_command(current_session_id, state).await),
        "/system-prompt" => Some(handle_system_prompt_command(current_session_id, state).await),
        "/mcp" => Some(handle_mcp_command_with_arg(arg, current_session_id, state).await),
        "/usage" => Some(handle_usage_command(current_session_id, state).await),
        "/clear" => Some(handle_clear_command(current_session_id, state).await),
        "/skills" => Some(handle_skills_command(None, current_session_id, state).await),
        "/skills-system" => {
            Some(handle_skills_system_command(arg, current_session_id, state).await)
        }
        "/skills-global" => Some(
            handle_skills_command(
                Some(prompts::SkillSource::Global),
                current_session_id,
                state,
            )
            .await,
        ),
        "/skills-session" => Some(
            handle_skills_command(
                Some(prompts::SkillSource::Session),
                current_session_id,
                state,
            )
            .await,
        ),
        "/think" => Some(handle_think_command(arg, current_session_id, state).await),
        "/react" => Some(handle_react_command(arg, current_session_id, state).await),
        "/tool" => Some(handle_tool_command(arg, current_session_id, state).await),
        "/reasoning" => Some(handle_reasoning_command(arg, current_session_id, state).await),
        "/help" => Some(handle_help_command(current_session_id)),
        "/sessions" => Some(handle_sessions_command(current_session_id, state).await),
        "/delete" => Some(handle_delete_command(arg, current_session_id, state).await),
        "/memory" => Some(handle_memory_command(arg, current_session_id, state).await),
        "/reflection" => Some(handle_reflection_command(arg, current_session_id, state).await),
        "/agents" => Some(handle_agents_command(current_session_id, state).await),

        // /stop when not busy — the in-flight case is handled by the agent loop drain
        "/stop" => Some(command_result("No active run to stop.", "system", false)),

        _ => None,
    }
}

#[cfg(test)]
#[path = "tests/commands_tests.rs"]
mod tests;
